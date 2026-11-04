// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License v3 (AGPLv3).
// See <https://www.gnu.org/licenses/agpl-3.0.html> for details.
//
// Copyright (c) 2025-2026 TensorChord Inc.
// Copyright (c) 2026 PGSTY PTE. LTD.
// Maintained by Ruohang Feng @ PGSTY (rh@vonng.com)

use clap::{Args, Parser, Subcommand};
use object::{Object, ObjectSection};
use pgrx_sql_entity_graph::{ControlFile, PgrxSql, SqlGraphEntity};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Build(BuildArgs),
    Clippy(ClippyArgs),
    /// Emit install SQL from an already compiled extension's schema section.
    Schema {
        library: PathBuf,
    },
}

#[derive(Args)]
struct BuildArgs {
    #[arg(short, long, default_value = "./build")]
    output: String,
    #[arg(long, default_value = target_tuple::TARGET, env = "TARGET")]
    target: String,
    #[arg(long, default_value = "release", env = "PROFILE")]
    profile: String,
    #[arg(long, env = "RUNNER")]
    runner: Option<String>,
}

#[derive(Args)]
struct ClippyArgs {
    #[arg(long, default_value = target_tuple::TARGET, env = "TARGET")]
    target: String,
    #[arg(long, default_value = "release", env = "PROFILE")]
    profile: String,
}

struct RustcCfg {
    is_macos: bool,
    is_windows: bool,
    is_emscripten: bool,
    is_unix: bool,
}

impl RustcCfg {
    fn dll_prefix(&self) -> Result<&'static str, Box<dyn Error>> {
        if self.is_macos {
            Ok("lib")
        } else if self.is_windows || self.is_emscripten {
            Ok("")
        } else if self.is_unix {
            Ok("lib")
        } else {
            Err("unknown operating system".into())
        }
    }
    fn dll_suffix(&self) -> Result<&'static str, Box<dyn Error>> {
        if self.is_macos {
            Ok(".dylib")
        } else if self.is_windows {
            Ok(".dll")
        } else if self.is_emscripten {
            Ok(".wasm")
        } else if self.is_unix {
            Ok(".so")
        } else {
            Err("unknown operating system".into())
        }
    }
    fn ext_suffix(&self, fork: &str) -> Result<&'static str, Box<dyn Error>> {
        if self.is_macos {
            Ok(if matches!(fork, "pg14" | "pg15") {
                ".so"
            } else {
                ".dylib"
            })
        } else if self.is_windows {
            Ok(".dll")
        } else if self.is_emscripten || self.is_unix {
            Ok(".so")
        } else {
            Err("unknown operating system".into())
        }
    }
}

#[derive(Deserialize)]
struct CargoMetadata {
    target_directory: String,
}

fn pg_config(pg_config: impl AsRef<Path>) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let mut command = Command::new(pg_config.as_ref());
    command.stderr(Stdio::inherit());
    eprintln!("Running {command:?}");
    let command_output = command.output()?;
    let command_status = command_output.status;
    if !command_status.success() {
        return Err(format!("PostgreSQL failed: {command_status}").into());
    }
    let contents = String::from_utf8(command_output.stdout)?;
    let mut result = HashMap::new();
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(" = ") {
            result.insert(key.to_string(), value.to_string());
        }
    }
    Ok(result)
}

fn control_file(path: impl AsRef<Path>) -> Result<HashMap<String, String>, Box<dyn Error>> {
    let path = path.as_ref();
    eprintln!("Reading {path:?}");
    let contents = std::fs::read_to_string(path)?;
    let mut result = HashMap::new();
    for line in contents.lines() {
        if let Some((key, prefix_stripped)) = line.split_once(" = '")
            && let Some(value) = prefix_stripped.strip_suffix("'")
        {
            result.insert(key.to_string(), value.to_string());
        }
    }
    Ok(result)
}

fn rustc_cfg(target: &str) -> Result<RustcCfg, Box<dyn Error>> {
    let mut command = Command::new("rustc");
    command
        .args(["--print", "cfg"])
        .args(["--target", target])
        .stderr(Stdio::inherit());
    eprintln!("Running {command:?}");
    let command_output = command.output()?;
    let command_status = command_output.status;
    if !command_status.success() {
        return Err(format!("Rust failed: {command_status}").into());
    }
    let contents = String::from_utf8(command_output.stdout)?;
    let mut cfgs = HashSet::new();
    for line in contents.lines() {
        cfgs.insert(line.to_string());
    }
    Ok(RustcCfg {
        is_macos: cfgs.contains("target_os=\"macos\""),
        is_unix: cfgs.contains("target_family=\"unix\""),
        is_emscripten: cfgs.contains("target_os=\"emscripten\""),
        is_windows: cfgs.contains("target_os=\"windows\""),
    })
}

fn cargo_metadata() -> Result<CargoMetadata, Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command
        .args(["metadata", "--format-version", "1"])
        .stderr(Stdio::inherit());
    eprintln!("Running {command:?}");
    let command_output = command.output()?;
    let command_status = command_output.status;
    if !command_status.success() {
        return Err(format!("Cargo failed: {command_status}").into());
    }
    let contents = String::from_utf8(command_output.stdout)?;
    let cargo_metadata: CargoMetadata = serde_json::from_str(&contents)?;
    Ok(cargo_metadata)
}

fn build(
    pg_config: impl AsRef<Path>,
    pg_version: &str,
    rustc_cfg: &RustcCfg,
    cargo_metadata: &CargoMetadata,
    profile: &str,
    target: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command.args(["rustc"]);
    if !matches!(profile, "dev" | "test") {
        command.args(["--crate-type", "cdylib"]);
    }
    command
        .args(["-p", "vchord_bm25", "--lib"])
        .args(["--profile", profile])
        .args(["--target", target])
        .args(["--features", pg_version])
        .env("PGRX_PG_CONFIG_PATH", pg_config.as_ref())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    eprintln!("Running {command:?}");
    let command_status = command.spawn()?.wait()?;
    if !command_status.success() {
        return Err(format!("Cargo failed: {command_status}").into());
    }
    let mut result = PathBuf::from(&cargo_metadata.target_directory);
    result.push(target);
    result.push(match profile {
        "dev" | "test" => "debug",
        "release" | "bench" => "release",
        profile => profile,
    });
    result.push(format!(
        "{}vchord_bm25{}",
        rustc_cfg.dll_prefix()?,
        rustc_cfg.dll_suffix()?
    ));
    Ok(result)
}

fn generate(obj: impl AsRef<Path>) -> Result<String, Box<dyn Error>> {
    let obj = obj.as_ref();
    eprintln!("Reading {obj:?}");
    let contents = std::fs::read(obj)?;
    let object = object::File::parse(contents.as_slice())?;
    let mut section_data = None;
    for section in object.sections() {
        let Ok(name) = section.name() else {
            continue;
        };
        if pgrx_sql_entity_graph::section::is_schema_section_name(name) {
            section_data = Some(section.uncompressed_data()?);
            break;
        }
    }
    let section_data = section_data.ok_or("schema section not found in shared library")?;
    let mut entities = vec![SqlGraphEntity::ExtensionRoot(ControlFile::try_from(
        PathBuf::from("./vchord_bm25.control"),
    )?)];
    entities.extend(
        pgrx_sql_entity_graph::section::decode_entities(section_data.as_ref())
            .map_err(|e| e.to_string())?,
    );
    let pgrx_sql = PgrxSql::build(entities.into_iter(), "vchord_bm25".to_string(), false)
        .map_err(|e| e.to_string())?;
    let sql = pgrx_sql
        .to_sql()
        .map_err(|e| e.to_string())?
        .replace("\t", "    ");
    Ok(sql)
}

// pgrx includes source line numbers and emits comment-only graph nodes in an
// unstable order. Ignore these annotations when checking the release schema,
// but preserve quoted SQL verbatim: whitespace and comment markers in function
// bodies, literals and identifiers can change the API's behavior.
fn schema_sql(sql: &str) -> String {
    let mut result = String::new();
    let mut remaining = sql;
    while let Some(first) = remaining.chars().next() {
        let mut skip = first.len_utf8();
        let mut whitespace = first.is_whitespace();
        if remaining.starts_with("--") {
            skip = remaining.find('\n').unwrap_or(remaining.len());
            whitespace = true;
        } else if remaining.starts_with("/*") {
            let mut depth = 1;
            skip = 2;
            while depth > 0 && skip < remaining.len() {
                if remaining[skip..].starts_with("/*") {
                    depth += 1;
                    skip += 2;
                } else if remaining[skip..].starts_with("*/") {
                    depth -= 1;
                    skip += 2;
                } else {
                    skip += remaining[skip..].chars().next().unwrap().len_utf8();
                }
            }
            // Preserve malformed comments so they cannot mask schema drift.
            whitespace = depth == 0;
        } else if matches!(first, '\'' | '"') {
            let mut prefix = sql[..sql.len() - remaining.len()].chars().rev();
            let escapes = first == '\''
                && matches!(prefix.next(), Some('e' | 'E'))
                && prefix
                    .next()
                    .is_none_or(|c| !(c == '_' || c == '$' || c.is_alphanumeric()));
            while skip < remaining.len() {
                let next = remaining[skip..].chars().next().unwrap();
                skip += next.len_utf8();
                if next == first {
                    if remaining[skip..].starts_with(first) {
                        skip += first.len_utf8();
                    } else {
                        break;
                    }
                } else if next == '\\' && escapes {
                    // Also retain escaped quotes in E'...' function bodies.
                    if let Some(escaped) = remaining[skip..].chars().next() {
                        skip += escaped.len_utf8();
                    }
                }
            }
        } else if first == '$'
            && let Some(end) = remaining[1..].find('$')
        {
            let tag = &remaining[1..=end];
            if tag.is_empty()
                || (tag.starts_with(|c: char| c == '_' || c.is_alphabetic())
                    && tag.chars().all(|c| c == '_' || c.is_alphanumeric()))
            {
                let delimiter = &remaining[..end + 2];
                skip = remaining[delimiter.len()..]
                    .find(delimiter)
                    .map_or(remaining.len(), |end| end + 2 * delimiter.len());
            }
        }
        if whitespace {
            if !result.ends_with(' ') {
                result.push(' ');
            }
        } else {
            result.push_str(&remaining[..skip]);
        }
        remaining = &remaining[skip..];
    }
    result.trim().to_owned()
}

fn install_by_copying(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
    #[cfg_attr(not(target_family = "unix"), expect(unused_variables))] is_executable: bool,
) -> Result<(), Box<dyn Error>> {
    eprintln!("Copying {:?} to {:?}", src.as_ref(), dst.as_ref());
    std::fs::copy(src, &dst)?;
    #[cfg(target_family = "unix")]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        let perm = Permissions::from_mode(if !is_executable { 0o644 } else { 0o755 });
        std::fs::set_permissions(dst, perm)?;
    }
    Ok(())
}

fn install_by_writing(
    contents: impl AsRef<[u8]>,
    dst: impl AsRef<Path>,
    #[cfg_attr(not(target_family = "unix"), expect(unused_variables))] is_executable: bool,
) -> Result<(), Box<dyn Error>> {
    eprintln!("Writing {:?}", dst.as_ref());
    std::fs::write(&dst, contents)?;
    #[cfg(target_family = "unix")]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        let perm = Permissions::from_mode(if !is_executable { 0o644 } else { 0o755 });
        std::fs::set_permissions(dst, perm)?;
    }
    Ok(())
}

fn clippy(
    pg_config: impl AsRef<Path>,
    pg_version: &str,
    profile: &str,
    target: &str,
) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new("cargo");
    command
        .args(["clippy"])
        .args(["--workspace"])
        .args(["--profile", profile])
        .args(["--target", target])
        .args(["--features", pg_version])
        .env("PGRX_PG_CONFIG_PATH", pg_config.as_ref())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    eprintln!("Running {command:?}");
    let command_status = command.spawn()?.wait()?;
    if !command_status.success() {
        return Err(format!("Cargo failed: {command_status}").into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("PGRX_PG_CONFIG_PATH");
    }
    let cli = Cli::parse();
    match cli.command {
        Commands::Schema { library } => {
            print!("{}", generate(library)?);
        }
        Commands::Build(BuildArgs {
            output,
            target,
            profile,
            runner: _,
        }) => {
            if !std::fs::exists("./vchord_bm25.control")? {
                return Err("The script must be run from the vchord_bm25 source directory.".into());
            }
            let vchord_bm25_version =
                control_file("./vchord_bm25.control")?["default_version"].clone();
            let path = if let Some(value) = std::env::var_os("PG_CONFIG") {
                PathBuf::from(value)
            } else {
                return Err("Environment variable `PG_CONFIG` is not set.".into());
            };
            let pg_config = pg_config(&path)?;
            let pg_version = {
                let version = pg_config["VERSION"].clone();
                if let Some(prefix_stripped) = version.strip_prefix("PostgreSQL ") {
                    if let Some((stripped, _)) =
                        prefix_stripped.split_once(|c: char| !c.is_ascii_digit())
                    {
                        format!("pg{stripped}",)
                    } else {
                        format!("pg{prefix_stripped}",)
                    }
                } else {
                    return Err("PostgreSQL version is invalid.".into());
                }
            };
            let rustc_cfg = rustc_cfg(&target)?;
            let cargo_metadata = cargo_metadata()?;
            let obj = build(
                &path,
                &pg_version,
                &rustc_cfg,
                &cargo_metadata,
                &profile,
                &target,
            )?;
            let generated_sql = generate(&obj)?;
            if vchord_bm25_version != "0.0.0" {
                let install_sql = format!("./sql/install/vchord_bm25--{vchord_bm25_version}.sql");
                if schema_sql(&std::fs::read_to_string(&install_sql)?) != schema_sql(&generated_sql)
                {
                    return Err(format!(
                        "{install_sql} does not match the compiled extension. Regenerate the install SQL before packaging."
                    ).into());
                }
            }
            let pkglibdir = format!("{output}/pkglibdir");
            let sharedir = format!("{output}/sharedir");
            let sharedir_extension = format!("{sharedir}/extension");
            if std::fs::exists(&output)? {
                std::fs::remove_dir_all(&output)?;
            }
            std::fs::create_dir_all(&output)?;
            std::fs::create_dir_all(&pkglibdir)?;
            std::fs::create_dir_all(&sharedir)?;
            std::fs::create_dir_all(&sharedir_extension)?;
            install_by_copying(
                &obj,
                format!(
                    "{pkglibdir}/vchord_bm25{}",
                    rustc_cfg.ext_suffix(&pg_version)?
                ),
                true,
            )?;
            install_by_copying(
                "./vchord_bm25.control",
                format!("{sharedir}/extension/vchord_bm25.control"),
                false,
            )?;
            if vchord_bm25_version != "0.0.0" {
                for e in std::fs::read_dir("./sql/upgrade")?.collect::<Result<Vec<_>, _>>()? {
                    install_by_copying(
                        e.path(),
                        format!("{sharedir}/extension/{}", e.file_name().display()),
                        false,
                    )?;
                }
                install_by_copying(
                    format!("./sql/install/vchord_bm25--{vchord_bm25_version}.sql"),
                    format!("{sharedir}/extension/vchord_bm25--{vchord_bm25_version}.sql"),
                    false,
                )?;
            } else {
                install_by_writing(
                    generated_sql,
                    format!("{sharedir_extension}/vchord_bm25--0.0.0.sql"),
                    false,
                )?;
            }
        }
        Commands::Clippy(ClippyArgs { target, profile }) => {
            if !std::fs::exists("./vchord_bm25.control")? {
                return Err("The script must be run from the vchord_bm25 source directory.".into());
            }
            let path = if let Some(value) = std::env::var_os("PG_CONFIG") {
                PathBuf::from(value)
            } else {
                return Err("Environment variable `PG_CONFIG` is not set.".into());
            };
            let pg_config = pg_config(&path)?;
            let pg_version = {
                let version = pg_config["VERSION"].clone();
                if let Some(prefix_stripped) = version.strip_prefix("PostgreSQL ") {
                    if let Some((stripped, _)) =
                        prefix_stripped.split_once(|c: char| !c.is_ascii_digit())
                    {
                        format!("pg{stripped}",)
                    } else {
                        format!("pg{prefix_stripped}",)
                    }
                } else {
                    return Err("PostgreSQL version is invalid.".into());
                }
            };
            clippy(&path, &pg_version, &profile, &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::schema_sql;

    #[test]
    fn schema_ignores_generated_annotations() {
        assert_eq!(
            schema_sql(
                "/* graph */\n-- src/lib.rs:39\nCREATE  TYPE /* Rust /* nested */ */ bm25query;"
            ),
            schema_sql("CREATE TYPE bm25query;"),
        );
    }

    #[test]
    fn schema_preserves_literal_and_function_body_changes() {
        for quoted in [
            "'two  spaces /* literal */ -- literal'",
            "\"two  spaces /* identifier */\"",
            "$$SELECT 'two  spaces'; -- body comment\n$$",
            "$body$SELECT 'two  spaces'; /* body comment */$body$",
            "E'escaped\\'two  spaces'",
            "'doubled''two  spaces'",
            r"'backslash\', 'two  spaces'",
            r"e'escaped\'two  spaces'",
            r"some'backslash\', 'two  spaces'",
        ] {
            let sql = format!("SELECT {quoted};");
            assert_eq!(schema_sql(&sql), sql);
            assert_ne!(schema_sql(&sql), schema_sql(&sql.replace("two  ", "two ")));
        }
        assert_ne!(
            schema_sql("SELECT 1; /* unterminated"),
            schema_sql("SELECT 1;")
        );
    }
}
