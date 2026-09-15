//! Read installed distribution metadata in the selected Python interpreter.
//! The inspector runs isolated from the project and never installs packages.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const OUTPUT_MARKER: &str = "PIE_CRUST_DEPENDENCIES_V1=";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct DependencyStatus {
    pub requirement: String,
    pub installed: Option<String>,
    pub status: String,
    pub detail: String,
}

const INSPECTOR: &str = r#"
from importlib import metadata
try:
    from packaging.requirements import Requirement
except ImportError:
    try:
        from pip._vendor.packaging.requirements import Requirement
    except ImportError:
        Requirement = None

results = []
for requirement in requirements:
    row = dict(requirement=requirement, installed=None, status='unknown', detail='')
    try:
        if Requirement is None:
            row['detail'] = 'Analyse indisponible : packaging et pip sont absents de cet environnement.'
        else:
            parsed = Requirement(requirement)
            if parsed.marker is not None and not parsed.marker.evaluate({'extra': ''}):
                row['status'] = 'skipped'
                row['detail'] = "Le marqueur de cette dependance ne correspond pas a cet environnement."
            else:
                try:
                    row['installed'] = metadata.version(parsed.name)
                except metadata.PackageNotFoundError:
                    pass
                if row['installed'] is None:
                    row['status'] = 'missing'
                    row['detail'] = 'Distribution absente de cet environnement.'
                elif parsed.url is not None:
                    row['detail'] = "Distribution installee ; la provenance de l'URL ne peut pas etre confirmee."
                elif parsed.specifier.contains(row['installed'], prereleases=True):
                    row['status'] = 'ok'
                    row['detail'] = 'Version installee compatible avec la declaration.'
                else:
                    row['status'] = 'mismatch'
                    row['detail'] = 'La version installee ne respecte pas la contrainte declaree.'
    except Exception as error:
        row['status'] = 'unknown'
        row['detail'] = 'Verification impossible : ' + str(error)
    results.append(row)
print('PIE_CRUST_DEPENDENCIES_V1=' + json.dumps(results, ensure_ascii=True, separators=(',', ':')))
"#;

pub(super) fn inspection_args(dependencies: &[String]) -> String {
    let script = inspection_script(dependencies);
    python_script_args(&script)
}

/// Runs a package manager against this interpreter even when its venv has no pip.
/// Read operations never bootstrap/install a package manager into the environment.
pub(super) fn package_manager_args(upgrade: Option<&str>) -> String {
    python_script_args(&package_manager_script(upgrade))
}

fn package_manager_script(upgrade: Option<&str>) -> String {
    let arguments = match upgrade {
        Some(name) => vec!["install", "--upgrade", name],
        None => vec!["list", "--outdated", "--format=json"],
    };
    let arguments = serde_json::to_string(&arguments).expect("package arguments serialize");
    let literal = serde_json::to_string(&arguments).expect("JSON serializes as a string");
    format!("import json\narguments = json.loads({literal})\n{PACKAGE_MANAGER}")
}

const PACKAGE_MANAGER: &str = r#"
import importlib.util
import shutil
import subprocess
import sys

if importlib.util.find_spec('pip') is not None:
    command = [sys.executable, '-I', '-B', '-m', 'pip', '--disable-pip-version-check', *arguments]
    manager = 'pip'
elif shutil.which('uv'):
    command = [shutil.which('uv'), 'pip', arguments[0], '--python', sys.executable,
               '--no-python-downloads', *arguments[1:]]
    manager = 'uv'
elif shutil.which('pip'):
    command = [shutil.which('pip'), '--python', sys.executable,
               '--disable-pip-version-check', *arguments]
    manager = 'pip externe'
else:
    print('Aucun gestionnaire de paquets disponible : ce venv ne contient pas pip, '
          'et uv/pip sont absents du PATH. Installez uv ou pip pour activer cette action.', file=sys.stderr)
    sys.exit(2)
print('Gestionnaire : ' + manager + ' | Python : ' + sys.executable, flush=True)
sys.exit(subprocess.run(command, stdin=sys.stdin, stdout=sys.stdout, stderr=sys.stderr,
                        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0)).returncode)
"#;

fn python_script_args(script: &str) -> String {
    // Hex keeps every project-controlled character out of shell syntax. The
    // wrapper also avoids double quotes in native Windows argument forwarding.
    let encoded: String = script
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let command = format!("exec(bytes.fromhex('{encoded}').decode('utf-8'))");
    format!("-I -B -c {}", quote_shell_argument(&command, cfg!(windows)))
}

fn inspection_script(dependencies: &[String]) -> String {
    let requirements = serde_json::to_string(dependencies).expect("requirements serialize as JSON");
    let literal = serde_json::to_string(&requirements).expect("JSON serializes as a string");
    format!("import json\nrequirements = json.loads({literal})\n{INSPECTOR}")
}

fn quote_shell_argument(argument: &str, windows: bool) -> String {
    if windows {
        format!("'{}'", argument.replace('\'', "''"))
    } else {
        format!("'{}'", argument.replace('\'', "'\"'\"'"))
    }
}

pub(super) fn parse_inspection(output: &str) -> Result<Vec<DependencyStatus>> {
    let json = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(OUTPUT_MARKER))
        .context(
            "L'interpreteur n'a pas retourne le resultat de la verification des dependances",
        )?;
    let rows: Vec<DependencyStatus> =
        serde_json::from_str(json).context("Resultat de verification des dependances invalide")?;
    if rows.iter().any(|row| {
        !matches!(
            row.status.as_str(),
            "ok" | "missing" | "mismatch" | "skipped" | "unknown"
        )
    }) {
        bail!("Etat de dependance inconnu dans le resultat de verification");
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manager_fallback_keeps_target_and_never_bootstraps_pip() {
        let Some(interpreter) = std::env::var_os("PIE_CRUST_TEST_PYTHON") else {
            return;
        };
        let prefix = r#"
import sys, json, types
from unittest.mock import patch
scripts = SCRIPTS
for script in scripts:
    for mode in ['pip', 'uv', 'external', 'missing']:
        calls = []
        def which(name):
            if mode == 'uv' and name == 'uv': return '/tool with spaces/uv'
            if mode == 'external' and name == 'pip': return '/tools/pip'
            return None
        def run(command, **kwargs):
            calls.append(command)
            return types.SimpleNamespace(returncode=7)
        with patch('importlib.util.find_spec', return_value=object() if mode == 'pip' else None), patch('shutil.which', side_effect=which), patch('subprocess.run', side_effect=run):
            try:
                exec(script, {})
                raise AssertionError('expected exit')
            except SystemExit as exit:
                assert exit.code == (2 if mode == 'missing' else 7), (mode, exit.code)
        if mode == 'missing':
            assert not calls
            continue
        command, = calls
        assert sys.executable in command
        assert 'ensurepip' not in command
        assert '--upgrade' not in command or command[-1] == 'example-package'
        if mode == 'pip':
            assert command[:5] == [sys.executable, '-I', '-B', '-m', 'pip']
        else:
            assert command[command.index('--python') + 1] == sys.executable
        if mode == 'uv':
            assert command[0] == '/tool with spaces/uv'
            assert '--no-python-downloads' in command
        if mode == 'external': assert command[0] == '/tools/pip'
"#;
        let scripts = serde_json::to_string(&[
            package_manager_script(None),
            package_manager_script(Some("example-package")),
        ])
        .unwrap();
        let literal = serde_json::to_string(&scripts).unwrap();
        let script = prefix.replace("SCRIPTS", &format!("json.loads({literal})"));
        let output = std::process::Command::new(interpreter)
            .args(["-I", "-B", "-c", &script])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn uv_reads_the_real_environment_offline_without_pip() {
        if std::env::var_os("PIE_CRUST_TEST_UV").is_none() {
            return;
        }
        let interpreter =
            std::env::var_os("PIE_CRUST_TEST_PYTHON").expect("set PIE_CRUST_TEST_PYTHON");
        let script =
            format!("arguments = ['list', '--format=json', '--offline']\n{PACKAGE_MANAGER}");
        let output = std::process::Command::new(interpreter)
            .args(["-I", "-B", "-c", &script])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = std::str::from_utf8(&output.stdout).unwrap();
        assert!(stdout.contains("Gestionnaire : uv"), "{stdout}");
        let json_start = stdout.find('[').expect("installed list JSON");
        let rows: serde_json::Value = serde_json::from_str(stdout[json_start..].trim()).unwrap();
        assert!(!rows.as_array().unwrap().is_empty());
    }

    #[test]
    fn actual_outdated_query_works_without_installing_pip() {
        if std::env::var_os("PIE_CRUST_TEST_UPDATES").is_none() {
            return;
        }
        let interpreter =
            std::env::var_os("PIE_CRUST_TEST_PYTHON").expect("set PIE_CRUST_TEST_PYTHON");
        let output = std::process::Command::new(interpreter)
            .args(["-I", "-B", "-c", &package_manager_script(None)])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = std::str::from_utf8(&output.stdout).unwrap();
        let json_start = stdout.find('[').expect("outdated list JSON");
        let rows: serde_json::Value = serde_json::from_str(stdout[json_start..].trim()).unwrap();
        assert!(rows.is_array());
        assert!(stdout.contains("Gestionnaire : uv"), "{stdout}");
        println!(
            "uv a vérifié le venv sans pip : {} mises à jour disponibles",
            rows.as_array().unwrap().len()
        );
    }

    #[test]
    fn parser_uses_last_result_amid_terminal_noise() {
        let output = "Launching interpreter\r\nPIE_CRUST_DEPENDENCIES_V1=[]\r\nwarning: metadata notice\r\nPIE_CRUST_DEPENDENCIES_V1=[{\"requirement\":\"requests>=2\",\"installed\":\"2.32.0\",\"status\":\"ok\",\"detail\":\"compatible\"}]\r\nProcess completed\r\n";
        let rows = parse_inspection(output).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requirement, "requests>=2");
        assert_eq!(rows[0].installed.as_deref(), Some("2.32.0"));
        assert_eq!(rows[0].status, "ok");
    }

    #[test]
    fn parser_rejects_missing_malformed_or_unknown_results() {
        assert!(parse_inspection("Process failed\n[]").is_err());
        assert!(parse_inspection("PIE_CRUST_DEPENDENCIES_V1=not json").is_err());
        assert!(
            parse_inspection("PIE_CRUST_DEPENDENCIES_V1=[]\nPIE_CRUST_DEPENDENCIES_V1=broken")
                .is_err()
        );
        assert!(parse_inspection("PIE_CRUST_DEPENDENCIES_V1=[{\"requirement\":\"x\",\"installed\":null,\"status\":\"guessed\",\"detail\":\"\"}]").is_err());
        assert!(
            parse_inspection("PIE_CRUST_DEPENDENCIES_V1=[]")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn shell_arguments_quote_apostrophes_for_powershell_and_posix() {
        let value = "package's $(command) `text` \"quote\"; next\nline";
        assert_eq!(
            quote_shell_argument(value, true),
            "'package''s $(command) `text` \"quote\"; next\nline'"
        );
        assert_eq!(
            quote_shell_argument(value, false),
            "'package'\"'\"'s $(command) `text` \"quote\"; next\nline'"
        );
    }

    #[test]
    fn requirement_contents_are_encoded_and_cannot_become_shell_commands() {
        let dependencies = vec![
            "requests>=2; python_version >= '3.10'".to_owned(),
            "bad\"); print('injected'); # $(touch file) `text`\nUnicode: é".to_owned(),
        ];
        let args = inspection_args(&dependencies);
        assert!(args.starts_with("-I -B -c "));
        assert!(!args.contains("requests"));
        assert!(!args.contains("injected"));
        assert!(!args.contains('$'));
        assert!(!args.contains('`'));
        assert!(!args.contains('\n'));
        let prefix = if cfg!(windows) {
            "fromhex(''"
        } else {
            "fromhex('\"'\"'"
        };
        let encoded = args
            .split_once(prefix)
            .unwrap()
            .1
            .split('\'')
            .next()
            .unwrap();
        let bytes = encoded
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let script = String::from_utf8(bytes).unwrap();
        let literal = script
            .lines()
            .nth(1)
            .unwrap()
            .strip_prefix("requirements = json.loads(")
            .unwrap()
            .strip_suffix(')')
            .unwrap();
        let json: String = serde_json::from_str(literal).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&json).unwrap(),
            dependencies
        );
        assert!(script.contains("from importlib import metadata"));
        assert!(!script.contains("pip install"));
    }

    /// Opt in with an absolute interpreter containing packaging or pip's
    /// vendored packaging. This reads metadata without importing cwd.
    #[test]
    fn isolated_inspector_checks_real_metadata_and_native_shell_quoting() {
        let Some(interpreter) = std::env::var_os("PIE_CRUST_TEST_PYTHON") else {
            return;
        };
        let interpreter = std::path::PathBuf::from(interpreter);
        assert!(
            interpreter.is_absolute(),
            "PIE_CRUST_TEST_PYTHON must be absolute"
        );
        assert!(interpreter.is_file(), "PIE_CRUST_TEST_PYTHON must exist");
        let cwd = tempfile::tempdir().unwrap();
        for module in ["json", "packaging", "pip", "importlib"] {
            std::fs::write(
                cwd.path().join(format!("{module}.py")),
                "raise AssertionError('The inspector imported the project directory')\n",
            )
            .unwrap();
        }
        let metadata_script = "import json\nfrom importlib import metadata\nfor name in ('packaging', 'Django', 'pip'):\n    try:\n        version = metadata.version(name)\n    except metadata.PackageNotFoundError:\n        continue\n    print(json.dumps([name, version]))\n    break\nelse:\n    raise SystemExit('Test environment needs packaging, Django, or pip metadata')\n";
        let metadata = std::process::Command::new(&interpreter)
            .args(["-I", "-B", "-c", metadata_script])
            .current_dir(cwd.path())
            .output()
            .unwrap();
        assert!(
            metadata.status.success(),
            "{}",
            String::from_utf8_lossy(&metadata.stderr)
        );
        let (package, installed): (String, String) =
            serde_json::from_slice(&metadata.stdout).unwrap();
        let dependencies = vec![
            format!("{package}>=0"),
            format!("{package}<0"),
            "pie_crust-no-such-distribution-92b8d1efc4>=1".to_owned(),
            format!("{package}; python_version < '0'"),
            "not a valid requirement ???".to_owned(),
            format!("{package} @ https://example.invalid/package.whl"),
            "broken \" $(Write-Output injected) `text` ' Unicode: é\nline".to_owned(),
        ];
        let output = std::process::Command::new(&interpreter)
            .args(["-I", "-B", "-c", &inspection_script(&dependencies)])
            .current_dir(cwd.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows = parse_inspection(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.requirement.as_str())
                .collect::<Vec<_>>(),
            dependencies.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.status.as_str())
                .collect::<Vec<_>>(),
            [
                "ok", "mismatch", "missing", "skipped", "unknown", "unknown", "unknown"
            ]
        );
        assert_eq!(rows[0].installed.as_deref(), Some(installed.as_str()));
        assert_eq!(rows[1].installed, rows[0].installed);
        assert_eq!(rows[5].installed, rows[0].installed);
        assert!(rows[2].installed.is_none());
        assert!(rows[3].installed.is_none());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let command = format!(
                "& {} {}",
                quote_shell_argument(interpreter.to_str().unwrap(), true),
                inspection_args(&dependencies),
            );
            assert!(command.encode_utf16().count() < 30_000);
            let output = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &command])
                .current_dir(cwd.path())
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let shell_rows =
                parse_inspection(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
            assert_eq!(shell_rows, rows);
        }
        assert!(!cwd.path().join("__pycache__").exists());
    }
}
