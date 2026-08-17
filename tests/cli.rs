use std::path::PathBuf;
use std::process::Command;

/// The suite is a shell script, because a courier is made entirely of
/// seams with other programs and those are what it has to be tested
/// against: a real beb, a real depot, a real sshd.
#[test]
fn e2e() {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sibling = |p: &str| here.parent().map(|d| d.join(p)).filter(|p| p.is_file());
    let mut cmd = Command::new("bash");
    cmd.arg(here.join("tests/e2e.sh"))
        .env("BEB_COURIER_BIN", env!("CARGO_BIN_EXE_beb-courier"));
    if let Some(p) = sibling("beb-depot/target/release/beb-depot") {
        cmd.env("BEB_DEPOT_BIN", p);
    }
    if let Some(p) = sibling("beb/target/release/beb") {
        cmd.env("BEB_BIN", p);
    }
    let out = cmd.output().expect("run e2e.sh");
    print!("{}", String::from_utf8_lossy(&out.stdout));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "e2e.sh failed");
}
