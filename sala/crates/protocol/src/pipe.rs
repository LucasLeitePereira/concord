//! Name of the IPC named pipe (D-010). Pure string logic, shared by core and UI.

/// Replaces anything that is not `[A-Za-z0-9_.-]` with `_`; never returns empty.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' })
        .collect();
    if out.is_empty() { "user".to_string() } else { out }
}

/// `\\.\pipe\sala-core-<user>` or `\\.\pipe\sala-core-<user>-<profile>` (D-010).
pub fn pipe_name(user: &str, profile: Option<&str>) -> String {
    match profile {
        Some(p) => format!(r"\\.\pipe\sala-core-{}-{}", sanitize(user), sanitize(p)),
        None => format!(r"\\.\pipe\sala-core-{}", sanitize(user)),
    }
}

/// [`pipe_name`] for the current Windows user (`USERNAME`).
pub fn current_user_pipe_name(profile: Option<&str>) -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    pipe_name(&user, profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_format_and_sanitize() {
        assert_eq!(pipe_name("ana", None), r"\\.\pipe\sala-core-ana");
        assert_eq!(pipe_name("ana", Some("b")), r"\\.\pipe\sala-core-ana-b");
        assert_eq!(pipe_name(r"DOM\ana smith", None), r"\\.\pipe\sala-core-DOM_ana_smith");
        assert_eq!(pipe_name("", Some("")), r"\\.\pipe\sala-core-user-user");
    }

    #[test]
    fn sanitize_blocks_path_tricks() {
        assert!(!pipe_name(r"..\..\x", None).trim_start_matches(r"\\.\pipe\").contains('\\'));
    }
}
