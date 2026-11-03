use longshipx_net_kit::tls::server_config_from_files;
use std::path::Path;

#[test]
fn certificate_paths_resolve_without_shell_expansion() {
    if let Ok(mode) = std::env::var("LONGSHIPX_TLS_PATH_TEST") {
        if mode == "paths" {
            let home = std::env::var_os("HOME").unwrap();
            assert!(
                server_config_from_files(
                    &Path::new(&home).join("certs/server.crt"),
                    &Path::new(&home).join("certs/server.key"),
                )
                .is_ok()
            );
            assert!(
                server_config_from_files(
                    Path::new("certs/server.crt"),
                    Path::new("certs/server.key"),
                )
                .is_ok()
            );
            let result = server_config_from_files(
                Path::new("~/certs/server.crt"),
                Path::new("~/certs/server.key"),
            );
            assert!(result.is_ok(), "tilde paths failed: {result:?}");
            assert!(
                server_config_from_files(
                    Path::new("~/certs/missing.crt"),
                    Path::new("~/certs/server.key"),
                )
                .is_err()
            );
        } else {
            let error =
                server_config_from_files(Path::new("~/cert"), Path::new("~/key")).unwrap_err();
            assert!(error.to_string().contains("HOME"), "{error}");
        }
        return;
    }

    let home = std::env::temp_dir().join(format!("longshipx-tls-paths-{}", std::process::id()));
    std::fs::create_dir_all(home.join("certs")).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    std::fs::write(home.join("certs/server.crt"), cert.pem()).unwrap();
    std::fs::write(home.join("certs/server.key"), key.serialize_pem()).unwrap();
    let mut failures = Vec::new();
    for mode in ["paths", "missing-home", "empty-home"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "certificate_paths_resolve_without_shell_expansion",
                "--nocapture",
            ])
            .current_dir(&home)
            .env("LONGSHIPX_TLS_PATH_TEST", mode);
        match mode {
            "paths" => command.env("HOME", &home),
            "empty-home" => command.env("HOME", ""),
            _ => command.env_remove("HOME"),
        };
        let output = command.output().unwrap();
        if !output.status.success() {
            failures.push(format!(
                "{mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    std::fs::remove_dir_all(home).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
