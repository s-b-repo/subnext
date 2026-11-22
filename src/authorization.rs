//! Authorization for session access. The operator policy supplies identities
//! and exact workspace scopes. A request supplies only its credential and target.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::hash::{Digest, sha256};

/// A verified credential scoped to one workspace and a fixed operation set.
#[derive(Clone)]
pub struct ScopedAuthToken {
    subject: String,
    workspace: String,
    write: bool,
    expires_unix: u64,
}

impl ScopedAuthToken {
    pub fn workspace(&self) -> &str { &self.workspace }

    fn allows(&self, operation: &str, workspace: &str, now: u64) -> bool {
        self.workspace == workspace && now < self.expires_unix
            && (operation == "ask" || (self.write && operation == "ingest"))
    }
}

fn now_unix() -> Option<u64> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|time| time.as_secs())
}

pub(crate) fn valid_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Operator-owned policy rows: `subject workspace read|write expires_unix sha256`.
/// The digest is of the 64-character credential text. The file is re-read at
/// authentication so an operator can revoke access from an open connection.
#[derive(Clone)]
pub struct SessionPolicy {
    path: PathBuf,
    stop_file: Option<PathBuf>,
}

impl SessionPolicy {
    pub fn open(path: PathBuf, stop_file: Option<PathBuf>) -> Result<Self, String> {
        let policy = Self { path, stop_file };
        policy.credentials()?;
        if !policy.running() {
            return Err("operator stopped the session service".into());
        }
        Ok(policy)
    }

    pub fn running(&self) -> bool {
        self.stop_file.as_deref().is_none_or(|path| {
            matches!(path.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        })
    }

    fn credentials(&self) -> Result<Vec<(Digest, ScopedAuthToken)>, String> {
        const MAX_POLICY_BYTES: u64 = 64 * 1024;
        let metadata = self.path.symlink_metadata().map_err(|e| format!("read session policy: {e}"))?;
        if !metadata.is_file() { return Err("session policy must be a regular private file".into()); }
        let file = File::open(&self.path).map_err(|e| format!("open session policy: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata().map_err(|e| format!("stat session policy: {e}"))?;
            if opened.mode() & 0o077 != 0 || opened.ino() != metadata.ino() || opened.dev() != metadata.dev() {
                return Err("session policy must be private (0600 or stricter)".into());
            }
        }
        let mut text = String::new();
        file.take(MAX_POLICY_BYTES + 1).read_to_string(&mut text).map_err(|e| format!("read session policy: {e}"))?;
        if text.len() as u64 > MAX_POLICY_BYTES { return Err("session policy exceeds the byte budget".into()); }
        let mut credentials = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') { continue; }
            let mut parts = line.split_whitespace();
            let row = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next());
            let (Some(subject), Some(workspace), Some(access @ ("read" | "write")), Some(expiry), Some(hash), None) = row else {
                return Err(format!("invalid session policy row {}", index + 1));
            };
            let expires_unix = expiry.parse::<u64>().map_err(|_| format!("invalid expiry in row {}", index + 1))?;
            let digest = Digest::parse_hex(hash).ok_or_else(|| format!("invalid digest in row {}", index + 1))?;
            if !valid_label(subject) || !valid_label(workspace) || credentials.iter().any(|(old, _)| old == &digest) {
                return Err(format!("invalid or duplicate credential in row {}", index + 1));
            }
            credentials.push((digest, ScopedAuthToken {
                subject: subject.into(), workspace: workspace.into(), write: access == "write", expires_unix,
            }));
        }
        Ok(credentials)
    }

    pub fn authenticate(&self, credential: &str) -> Option<ScopedAuthToken> {
        if !self.running() || credential.len() != 64 || !credential.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let supplied = sha256(credential.as_bytes());
        let current = now_unix()?;
        self.credentials().ok()?.into_iter().find_map(|(digest, identity)| {
            let diff = digest.as_bytes().iter().zip(supplied.as_bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b));
            (diff == 0 && current < identity.expires_unix).then_some(identity)
        })
    }
}

/// External policy decision point: authorization enforced outside the model.
#[derive(Debug, Clone, Default)]
pub struct PolicyDecisionPoint {
    pub decisions: Vec<String>,
}

impl PolicyDecisionPoint {
    pub fn decide(&mut self, credential: Option<&ScopedAuthToken>, tool: &str, workspace: &str) -> bool {
        let allowed = now_unix().zip(credential).is_some_and(|(now, cred)| cred.allows(tool, workspace, now));
        let subject = credential.map_or("anonymous", |cred| cred.subject.as_str());
        self.decisions.push(format!("{subject}:{tool}:{workspace}:{allowed}"));
        allowed
    }
}

pub type Pdp = PolicyDecisionPoint;
pub type ExternalPdp = PolicyDecisionPoint;

/// Policy enforcement point (the gate in front of tool execution).
#[derive(Debug, Clone, Default)]
pub struct PolicyEnforcementPoint {
    pub pdp: PolicyDecisionPoint,
}

impl PolicyEnforcementPoint {
    pub fn enforce(&mut self, credential: Option<&ScopedAuthToken>, tool: &str, workspace: &str) -> bool {
        self.pdp.decide(credential, tool, workspace)
    }
}

pub type Pep = PolicyEnforcementPoint;

pub type ContextAwareAuthorization = PolicyEnforcementPoint;
pub type FailClosedAuthorizer = PolicyEnforcementPoint;

/// Full audit logging of authorization decisions.
#[derive(Debug, Clone, Default)]
pub struct AuthorizationAuditLog {
    pub entries: Vec<String>,
}

impl AuthorizationAuditLog {
    pub fn log(&mut self, identity: &str, tool: &str, resource: &str, args: &str, decision: bool, reason: &str) {
        self.entries.push(format!(
            "identity={identity} tool={tool} resource={resource} args={args} decision={decision} reason={reason}"
        ));
    }

    pub fn record(&mut self, identity: &str, tool: &str, resource: &str, args: &str, decision: bool, reason: &str) {
        self.log(identity, tool, resource, args, decision, reason);
    }
}

pub type AuditLog = AuthorizationAuditLog;
pub type FullAuditLog = AuthorizationAuditLog;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_closed_default_deny() {
        let mut gate = FailClosedAuthorizer::default();
        assert!(!gate.enforce(None, "ask", "bench"));
    }

    #[test]
    fn audit_log_records() {
        let mut log = AuthorizationAuditLog::default();
        log.log("id", "tool", "res", "args", true, "ok");
        assert_eq!(log.entries.len(), 1);
    }

    #[test]
    fn only_the_current_workspace_and_operations_are_allowed() {
        let identity = ScopedAuthToken { subject: "tester".into(), workspace: "bench".into(), write: false, expires_unix: u64::MAX };
        let mut gate = PolicyEnforcementPoint::default();
        assert!(gate.enforce(Some(&identity), "ask", "bench"));
        assert!(!gate.enforce(Some(&identity), "ask", "other"));
        assert!(!gate.enforce(Some(&identity), "ingest", "bench"));
    }
}
