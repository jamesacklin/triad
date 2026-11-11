//! A host with no certificate store (the VM guest) falls back to the bundled
//! roots instead of failing. Its own test binary, because it changes the
//! process environment.

#[test]
fn falls_back_to_bundled_roots_without_a_store() {
    let empty = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary, run before any other thread
    // reads the environment.
    unsafe {
        std::env::set_var("SSL_CERT_FILE", empty.path().join("none.pem"));
        std::env::set_var("SSL_CERT_DIR", empty.path());
    }
    let (_, roots) = triad_agent::http::client().unwrap();
    assert_eq!(roots, triad_agent::http::Roots::Bundled);
}
