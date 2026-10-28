//! Who is acting. Ports the principal helpers of `src/vogt/core/`.

pub const LOCAL_SCHEME: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Principal {
    pub identity_ref: String,
    pub kind: ActorKind,
    pub display_name: String,
}

impl Principal {
    pub fn new(identity_ref: &str, kind: ActorKind, display_name: &str) -> Result<Self, String> {
        if identity_ref.trim().is_empty() {
            return Err("identity_ref must not be empty".to_string());
        }
        Ok(Self {
            identity_ref: identity_ref.to_string(),
            kind,
            display_name: display_name.to_string(),
        })
    }
}

pub fn local_principal(os_user: &str) -> Principal {
    Principal {
        identity_ref: format!("{LOCAL_SCHEME}:{os_user}"),
        kind: ActorKind::Human,
        display_name: os_user.to_string(),
    }
}

/// The name of the user who owns this process, the way `getpass.getuser()`
/// finds it: `LOGNAME`, then `USER`, then `LNAME`, then `USERNAME`, skipping a
/// variable that is unset or empty, and finally the passwd entry for the real
/// uid. `unknown` is what is left when none of those yields a name, which is
/// what `local_principal` does when `getpass` raises.
pub fn os_user() -> String {
    for name in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                return value;
            }
        }
    }
    passwd_name().unwrap_or_else(|| "unknown".to_string())
}

/// `pwd.getpwuid(os.getuid()).pw_name`. `None` when the uid has no entry.
/// Uses the `libc` crate's `passwd` so the struct layout matches the target
/// platform, and stops growing the buffer at 1 MiB.
fn passwd_name() -> Option<String> {
    const MAX_BUFFER: usize = 1 << 20;
    let mut buffer = vec![0u8; 1024];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    loop {
        // SAFETY: every pointer is valid for the call, and `buffer.len()` is
        // the length of the buffer passed with it.
        let code = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE && buffer.len() < MAX_BUFFER {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if code != 0 || result.is_null() {
            return None;
        }
        // SAFETY: on success `result` points at `entry`, whose `pw_name` is a
        // NUL-terminated string inside `buffer`, which is still alive here.
        let name = unsafe { std::ffi::CStr::from_ptr((*result).pw_name) };
        return name.to_str().ok().map(str::to_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_principal_needs_an_identity() {
        assert!(Principal::new("  ", ActorKind::Human, "nobody").is_err());
        let local = local_principal("sprooty");
        assert_eq!(local.identity_ref, "local:sprooty");
        assert_eq!(local.kind, ActorKind::Human);
    }
}
