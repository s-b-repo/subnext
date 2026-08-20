// Fixture for the section-P self-test in scripts/audit-selftest.sh.
//
// This file is NOT under src/, so it is never compiled and never scanned by the
// real gate. It exists solely so the self-test can prove section P still FIRES:
// the struct below derives `Debug` over a bare `secret` field, exactly the shape
// that leaked InsecureDevSigner's signing key. If the audit ever stops flagging
// this, the gate has silently broken — the defect this repo keeps re-finding.
#[derive(Debug)]
pub struct LeakyFixture {
    secret: String,
}
