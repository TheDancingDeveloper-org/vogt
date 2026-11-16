//! Test-only helper for fixtures that a test writes and then executes.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Write an executable script so that exec never sees a writable handle.
///
/// The script goes to a temp name, is synced and closed, then renamed into
/// place. A parallel test that forks while the write fd is open leaks that fd
/// into its child until the child execs, and exec of a file open for write
/// fails with ETXTBSY ("Text file busy"). So after the rename the file is
/// probe-run, retrying on ETXTBSY, until every leaked copy is gone.
pub(crate) fn write_executable(path: &Path, script: &str) {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.sync_all().unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(f);
    std::fs::rename(&tmp, path).unwrap();
    for _ in 0..500 {
        match std::process::Command::new(path).arg("probe").output() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            // Any other outcome means exec works; the probe's status is moot.
            _ => break,
        }
    }
}
