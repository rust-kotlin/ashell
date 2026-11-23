use super::{osc7_login_command, supports_posix_launcher};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[test]
fn login_shells_keep_user_hooks_and_report_only_changed_directories() {
    for shell in ["/bin/bash", "/bin/zsh"] {
        if !Path::new(shell).exists() {
            continue;
        }
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().canonicalize().unwrap();
        fs::create_dir(home.join("a 中#%")).unwrap();
        fs::write(
            home.join(".bash_profile"),
            r#"
PS1='test> '
user_prompt() { local saved=$?; printf '<user:%s>' "$saved"; return "$saved"; }
PROMPT_COMMAND=user_prompt
"#,
        )
        .unwrap();
        fs::write(
            home.join(".zshrc"),
            r#"
PS1='test> '
user_prompt() { local saved=$?; printf '<user:%s>' "$saved"; return "$saved"; }
precmd_functions=(user_prompt)
"#,
        )
        .unwrap();
        fs::write(home.join("system-profile"), "export ASHELL_TEST_LOGIN=ok\n").unwrap();
        fs::write(home.join(".zlogin"), "export ASHELL_TEST_LOGIN=ok\n").unwrap();
        let launch = osc7_login_command()
            .replace("/etc/profile", "\"$ASHELL_TEST_PROFILE\"")
            .replace("\"$SHELL\" -il", "\"$SHELL\" -dil");
        let mut child = Command::new("/bin/sh")
            .args(["-c", &launch])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("HOME", &home)
            .env("SHELL", shell)
            .env("ZDOTDIR", &home)
            .env("TEST_DIRECTORY", home.join("a 中#%"))
            .env("HISTFILE", "/dev/null")
            .env("ASHELL_TEST_PROFILE", home.join("system-profile"))
            .current_dir(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(
            b"cd \"$TEST_DIRECTORY\"; false\nfalse\nprintf '<rc=%s login=%s>' \"$?\" \"$ASHELL_TEST_LOGIN\"\ncd /\nexit 0\n",
        ).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let output = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<_> = output
            .split("\x1b]7;file://")
            .skip(1)
            .map(|part| part.split('\x07').next().unwrap())
            .collect();
        assert_eq!(output.matches("<user:").count(), 5, "{shell}: {output:?}");
        assert_eq!(reports.len(), 3, "{shell}: {output:?}");
        assert!(reports[1].ends_with("/a%20%E4%B8%AD%23%25"));
        assert_eq!(reports[2], "/");
        assert!(
            output.contains("<user:1>") && output.contains("<rc=1 login=ok>"),
            "{shell}: {output:?}"
        );
    }
}

#[test]
fn other_login_shells_do_not_get_bash_or_zsh_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let login = fixture.path().join("other-shell");
    fs::write(&login, "#!/bin/sh\nprintf 'login:%s' \"$1\"\n").unwrap();
    fs::set_permissions(&login, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new("/bin/sh")
        .args(["-c", &osc7_login_command()])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", &login)
        .current_dir(fixture.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"login:-l");
}

#[test]
fn unsafe_temp_paths_fall_back_before_writes_or_cleanup() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = tempfile::tempdir().unwrap();
    let login = fixture.path().join("bash");
    fs::write(&login, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
    fs::set_permissions(&login, fs::Permissions::from_mode(0o700)).unwrap();
    // Exercise the production guard with harmless stand-ins after the guard.
    let (guard, _) = include_str!("launch.sh")
        .split_once("readonly dir rc")
        .unwrap();
    let guard = guard.replace("/tmp/ashell-osc7.", r#""$TEST_ROOT"/ashell-osc7."#);
    let probe = format!("mktemp() {{ printf '%s' \"$CANDIDATE\"; }}\n{guard}\nprintf accepted");
    let run = |path: &Path| {
        Command::new("/bin/sh")
            .args(["-c", &probe])
            .env_clear()
            .env("TEST_ROOT", fixture.path())
            .env("SHELL", &login)
            .env("CANDIDATE", path)
            .output()
            .unwrap()
            .stdout
    };
    let valid = fixture.path().join("ashell-osc7.aB0cD123");
    fs::create_dir(&valid).unwrap();
    assert_eq!(run(&valid), b"accepted");
    let link = fixture.path().join("ashell-osc7.12345678");
    symlink(&valid, &link).unwrap();
    let dotted = fixture.path().join("ashell-osc7.bad.name");
    fs::create_dir(&dotted).unwrap();
    for path in [
        Path::new(""),
        Path::new("/"),
        fixture.path(),
        &link,
        &dotted,
        &fixture.path().join("ashell-osc7.ABCDEFGH"),
    ] {
        assert_eq!(run(path), b"-l", "{path:?}");
    }
}

#[test]
fn normal_exit_cleans_the_generated_startup_file_and_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let shell = fixture.path().join("bash");
    fs::write(&shell, "#!/bin/sh\n[ -f \"$ENV\" ] && printf %s \"$ENV\"\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new("/bin/sh")
        .args(["-c", &osc7_login_command()])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fixture.path())
        .env("SHELL", shell)
        .output()
        .unwrap();
    assert!(output.status.success());
    let init = String::from_utf8(output.stdout).unwrap();
    assert!(init.starts_with("/tmp/ashell-osc7.") && init.ends_with("/bash-init"));
    assert!(!Path::new(&init).parent().unwrap().exists());
}

#[test]
fn server_identification_selects_compatible_launchers() {
    for (server, supported) in [
        ("SSH-2.0-OpenSSH_9.6p1 Ubuntu", true),
        ("SSH-2.0-dropbear_2022.83", true),
        ("SSH-2.0-OpenSSH_for_Windows_9.5", false),
        ("SSH-2.0-network-appliance", false),
    ] {
        assert_eq!(
            supports_posix_launcher(server.as_bytes()),
            supported,
            "{server}"
        );
    }
}
