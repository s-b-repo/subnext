// Fixtures for the section-P self-test in scripts/audit-selftest.sh.
//
// NOT under src/, so never compiled and never scanned by the real gate. Each
// struct isolates one credential-field form the gate MUST catch; the self-test
// asserts ALL of them fire, so a regression in either the bare match or the
// compound (underscore) alternation drops the count and fails the build. A
// gate that only ever passes is indistinguishable from one whose regex rotted.
#[derive(Debug)]
pub struct BareSecret {
    secret: String,
}

#[derive(Debug)]
pub struct CompoundToken {
    access_token: String,
}

#[derive(Debug)]
pub struct CompoundSecret {
    client_secret: String,
}
