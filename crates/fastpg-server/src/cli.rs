use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use fastpg_server::DEFAULT_ADDR;

const POSTGRES_DEFAULT_HOST: &str = "127.0.0.1";
const POSTGRES_DEFAULT_PORT: &str = "5432";
const SOCKET_FILE_PREFIX: &str = ".s.PGSQL.";
const SUPPORTED_RUNTIME_SETTINGS: &[&str] =
    &["listen_addresses", "port", "unix_socket_directories"];
const SUPPORTED_QUERY_SETTINGS: &[&str] = &[
    "data_directory",
    "listen_addresses",
    "port",
    "unix_socket_directories",
];

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum StartupAction {
    Serve(ServerConfig),
    PrintAndExit(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ServerConfig {
    pub addr: String,
    pub pgdata: Option<PathBuf>,
    pub pid_file: Option<PidFileConfig>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PidFileConfig {
    pub path: PathBuf,
    pub data_dir: PathBuf,
    pub port: String,
    pub socket_dir: String,
    pub listen_addr: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct CliError {
    message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl fmt::Debug for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for CliError {}

trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CliMode {
    LegacyFastPg,
    Postgres,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct Settings {
    pgdata: Option<PathBuf>,
    port: Option<String>,
    listen_addresses: Option<String>,
    unix_socket_directories: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ListenerConfig {
    addr: String,
    port: String,
    socket_dir: String,
    listen_addr: String,
}

pub fn parse_cli() -> Result<StartupAction, CliError> {
    parse_from(env::args().collect::<Vec<_>>(), &ProcessEnv)
}

fn parse_from(args: Vec<String>, env: &impl EnvSource) -> Result<StartupAction, CliError> {
    let program = args.first().map(String::as_str).unwrap_or("fastpg-server");
    let rest = args.get(1..).unwrap_or_default();
    let mode = cli_mode(program, rest);

    match mode {
        CliMode::LegacyFastPg => parse_legacy_fastpg(rest, env),
        CliMode::Postgres => parse_postgres(program, rest, env),
    }
}

fn cli_mode(program: &str, args: &[String]) -> CliMode {
    if executable_name(program) == "postgres" || args.iter().any(|arg| arg.starts_with('-')) {
        CliMode::Postgres
    } else {
        CliMode::LegacyFastPg
    }
}

fn executable_name(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

fn parse_legacy_fastpg(args: &[String], env: &impl EnvSource) -> Result<StartupAction, CliError> {
    if args.len() > 1 {
        return Err(CliError::new(
            "fastpg-server accepts at most one positional listen address",
        ));
    }

    let addr = args
        .first()
        .cloned()
        .or_else(|| nonempty_env(env, "FASTPG_ADDR"))
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned());

    Ok(StartupAction::Serve(ServerConfig {
        addr,
        pgdata: normalized_pgdata(nonempty_env(env, "FASTPG_PGDATA"))?,
        pid_file: None,
    }))
}

fn parse_postgres(
    program: &str,
    args: &[String],
    env: &impl EnvSource,
) -> Result<StartupAction, CliError> {
    let mut cli_settings = Settings::default();
    let mut query_setting = None;
    let mut index = 0;

    cli_settings.pgdata = normalized_pgdata(
        nonempty_env(env, "PGDATA").or_else(|| nonempty_env(env, "FASTPG_PGDATA")),
    )?;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--help" || arg == "-?" {
            return Ok(StartupAction::PrintAndExit(help_text(program)));
        }
        if arg == "--version" || arg == "-V" {
            return Ok(StartupAction::PrintAndExit(version_text(program)));
        }
        if arg == "--describe-config"
            || arg == "--single"
            || arg == "--boot"
            || arg == "--check"
            || arg.starts_with("--forkchild")
        {
            return Err(unsupported_option(arg));
        }
        if let Some((name, value)) = parse_long_assignment(arg) {
            apply_supported_setting(&mut cli_settings, name, value)?;
            index += 1;
            continue;
        }
        if arg == "--" {
            if index + 1 < args.len() {
                return Err(CliError::new(format!(
                    "{program}: invalid argument: \"{}\"",
                    args[index + 1]
                )));
            }
            break;
        }
        if arg.starts_with("--") {
            return Err(unsupported_option(arg));
        }
        if !arg.starts_with('-') {
            return Err(CliError::new(format!(
                "{program}: invalid argument: \"{arg}\""
            )));
        }

        let option = short_option(arg)?;
        match option.name {
            'D' => {
                let value = option_argument(args, &mut index, option.value, "-D")?;
                cli_settings.pgdata = normalized_pgdata(Some(value.to_owned()))?;
            }
            'p' => {
                let value = option_argument(args, &mut index, option.value, "-p")?;
                cli_settings.port = Some(value.to_owned());
            }
            'h' => {
                let value = option_argument(args, &mut index, option.value, "-h")?;
                cli_settings.listen_addresses = Some(value.to_owned());
            }
            'i' => {
                reject_inline_value(option.value, "-i")?;
                cli_settings.listen_addresses = Some("*".to_owned());
            }
            'k' => {
                let value = option_argument(args, &mut index, option.value, "-k")?;
                cli_settings.unix_socket_directories = Some(value.to_owned());
            }
            'c' => {
                let value = option_argument(args, &mut index, option.value, "-c")?;
                let (name, value) = split_assignment(value, "-c")?;
                apply_supported_setting(&mut cli_settings, name, value)?;
            }
            'C' => {
                let value = option_argument(args, &mut index, option.value, "-C")?;
                let name = normalize_setting_name(value);
                if !SUPPORTED_QUERY_SETTINGS.contains(&name.as_str()) {
                    return Err(CliError::new(format!(
                        "unsupported PostgreSQL configuration setting for -C: {value}"
                    )));
                }
                query_setting = Some(name);
            }
            _ => return Err(unsupported_option(&format!("-{}", option.name))),
        }

        index += 1;
    }

    let config_settings = read_config_settings(cli_settings.pgdata.as_deref())?;
    let settings = merge_settings(config_settings, cli_settings);
    let listener = listener_config(&settings)?;

    if let Some(name) = query_setting {
        return Ok(StartupAction::PrintAndExit(format!(
            "{}\n",
            setting_value(&settings, &listener, &name)
        )));
    }

    let pid_file = settings.pgdata.as_ref().map(|pgdata| PidFileConfig {
        path: pgdata.join("postmaster.pid"),
        data_dir: pgdata.clone(),
        port: listener.port.clone(),
        socket_dir: listener.socket_dir.clone(),
        listen_addr: listener.listen_addr.clone(),
    });

    Ok(StartupAction::Serve(ServerConfig {
        addr: listener.addr,
        pgdata: settings.pgdata,
        pid_file,
    }))
}

#[derive(Debug, Clone, Copy)]
struct ShortOption<'a> {
    name: char,
    value: Option<&'a str>,
}

fn short_option(arg: &str) -> Result<ShortOption<'_>, CliError> {
    let mut chars = arg.chars();
    if chars.next() != Some('-') {
        return Err(CliError::new(format!("invalid option: {arg}")));
    }
    let Some(name) = chars.next() else {
        return Err(CliError::new("invalid option: -"));
    };
    let value = chars.as_str();
    let value = if value.is_empty() {
        None
    } else {
        Some(value.strip_prefix('=').unwrap_or(value))
    };
    Ok(ShortOption { name, value })
}

fn option_argument<'a>(
    args: &'a [String],
    index: &mut usize,
    inline: Option<&'a str>,
    option: &str,
) -> Result<&'a str, CliError> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| CliError::new(format!("{option} requires an argument")))
}

fn reject_inline_value(value: Option<&str>, option: &str) -> Result<(), CliError> {
    if value.is_some() {
        return Err(CliError::new(format!(
            "{option} does not accept an argument"
        )));
    }
    Ok(())
}

fn parse_long_assignment(arg: &str) -> Option<(&str, &str)> {
    let setting = arg.strip_prefix("--")?;
    let (name, value) = setting.split_once('=')?;
    Some((name, value))
}

fn split_assignment<'a>(assignment: &'a str, option: &str) -> Result<(&'a str, &'a str), CliError> {
    assignment
        .split_once('=')
        .ok_or_else(|| CliError::new(format!("{option} requires NAME=VALUE")))
}

fn apply_supported_setting(
    settings: &mut Settings,
    name: &str,
    value: &str,
) -> Result<(), CliError> {
    let name = normalize_setting_name(name);
    let value = unquote_setting_value(value.trim());
    match name.as_str() {
        "port" => settings.port = Some(value),
        "listen_addresses" => settings.listen_addresses = Some(value),
        "unix_socket_directories" => settings.unix_socket_directories = Some(value),
        _ => {
            return Err(CliError::new(format!(
                "unsupported PostgreSQL configuration setting: {name}"
            )));
        }
    }
    Ok(())
}

fn unsupported_option(option: &str) -> CliError {
    CliError::new(format!("unsupported PostgreSQL server option: {option}"))
}

fn normalize_setting_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn normalized_pgdata(pgdata: Option<String>) -> Result<Option<PathBuf>, CliError> {
    let Some(pgdata) = pgdata else {
        return Ok(None);
    };
    if pgdata.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(pgdata);
    if path.is_absolute() {
        Ok(Some(path))
    } else {
        Ok(Some(
            env::current_dir()
                .map_err(|error| {
                    CliError::new(format!("could not resolve current directory: {error}"))
                })?
                .join(path),
        ))
    }
}

fn nonempty_env(env: &impl EnvSource, key: &str) -> Option<String> {
    env.get(key).filter(|value| !value.is_empty())
}

fn read_config_settings(pgdata: Option<&Path>) -> Result<Settings, CliError> {
    let Some(pgdata) = pgdata else {
        return Ok(Settings::default());
    };
    let path = pgdata.join("postgresql.conf");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Ok(Settings::default());
    };
    parse_config_settings(&contents)
}

fn parse_config_settings(contents: &str) -> Result<Settings, CliError> {
    let mut settings = Settings::default();
    for line in contents.lines() {
        let stripped = strip_comment(line);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = normalize_setting_name(name);
        if !SUPPORTED_RUNTIME_SETTINGS.contains(&name.as_str()) {
            continue;
        }
        apply_supported_setting(&mut settings, &name, value.trim())?;
    }
    Ok(settings)
}

fn strip_comment(line: &str) -> String {
    let mut quote = None;
    let mut escaped = false;
    let mut result = String::new();

    for ch in line.chars() {
        if escaped {
            result.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            result.push(ch);
            continue;
        }
        if ch == '\'' || ch == '"' {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
            result.push(ch);
            continue;
        }
        if ch == '#' && quote.is_none() {
            break;
        }
        result.push(ch);
    }

    result
}

fn unquote_setting_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            let inner = &value[1..value.len() - 1];
            if first == b'\'' {
                return inner.replace("''", "'");
            }
            return inner.replace("\\\"", "\"");
        }
    }
    value.to_owned()
}

fn merge_settings(config: Settings, cli: Settings) -> Settings {
    Settings {
        pgdata: cli.pgdata.or(config.pgdata),
        port: cli.port.or(config.port),
        listen_addresses: cli.listen_addresses.or(config.listen_addresses),
        unix_socket_directories: cli
            .unix_socket_directories
            .or(config.unix_socket_directories),
    }
}

fn listener_config(settings: &Settings) -> Result<ListenerConfig, CliError> {
    let port = settings
        .port
        .clone()
        .unwrap_or_else(|| POSTGRES_DEFAULT_PORT.to_owned());
    validate_port(&port)?;

    let listen_addresses = settings
        .listen_addresses
        .clone()
        .unwrap_or_else(|| POSTGRES_DEFAULT_HOST.to_owned());
    let socket_dir = first_csv_value(settings.unix_socket_directories.as_deref())?;

    if listen_addresses.is_empty() {
        let Some(socket_dir) = socket_dir else {
            return Err(CliError::new(
                "listen_addresses disables TCP, but no Unix socket directory is configured",
            ));
        };
        let path = Path::new(socket_dir).join(format!("{SOCKET_FILE_PREFIX}{port}"));
        return Ok(ListenerConfig {
            addr: format!("unix:{}", path.display()),
            port,
            socket_dir: socket_dir.to_owned(),
            listen_addr: String::new(),
        });
    }

    if socket_dir.is_some() {
        return Err(CliError::new(
            "FastPG supports only one listener; TCP and Unix sockets were both requested",
        ));
    }

    let host = first_csv_value(Some(&listen_addresses))?
        .ok_or_else(|| CliError::new("listen_addresses is empty"))?;
    let bind_host = if host == "*" { "0.0.0.0" } else { host };
    Ok(ListenerConfig {
        addr: tcp_addr(bind_host, &port),
        port,
        socket_dir: String::new(),
        listen_addr: host.to_owned(),
    })
}

fn validate_port(port: &str) -> Result<(), CliError> {
    port.parse::<u16>()
        .map(|_| ())
        .map_err(|_| CliError::new(format!("invalid port for FastPG listener: {port}")))
}

fn first_csv_value(value: Option<&str>) -> Result<Option<&str>, CliError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.contains(',') {
        return Err(CliError::new(
            "FastPG supports only one listen address or socket directory",
        ));
    }
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn tcp_addr(host: &str, port: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn setting_value(settings: &Settings, listener: &ListenerConfig, name: &str) -> String {
    match name {
        "data_directory" => settings
            .pgdata
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        "port" => listener.port.clone(),
        "listen_addresses" => listener.listen_addr.clone(),
        "unix_socket_directories" => listener.socket_dir.clone(),
        _ => String::new(),
    }
}

fn help_text(program: &str) -> String {
    format!(
        "{program} is the FastPG PostgreSQL-compatible test server.\n\n\
Usage:\n  {program} [OPTION]...\n\n\
Supported options:\n\
  -D DATADIR         database directory\n\
  -h HOSTNAME        host name or IP address to listen on\n\
  -i                 listen on all TCP addresses\n\
  -k DIRECTORY       Unix-domain socket location\n\
  -p PORT            port number to listen on\n\
  -c NAME=VALUE      set supported run-time parameter\n\
  -C NAME            print supported run-time parameter, then exit\n\
  --NAME=VALUE       set supported run-time parameter\n\
  -V, --version      output version information, then exit\n\
  -?, --help         show this help, then exit\n\n\
Supported -c/--NAME parameters: listen_addresses, port, unix_socket_directories.\n\
Supported -C parameters: data_directory, listen_addresses, port, unix_socket_directories.\n"
    )
}

fn version_text(program: &str) -> String {
    if executable_name(program) == "postgres" {
        format!(
            "postgres (PostgreSQL) {}\n",
            fastpg_compat::DEFAULT_SERVER_VERSION
        )
    } else {
        format!("fastpg-server {}\n", env!("CARGO_PKG_VERSION"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct TestEnv {
        values: BTreeMap<String, String>,
    }

    impl TestEnv {
        fn with(mut self, key: &str, value: &str) -> Self {
            self.values.insert(key.to_owned(), value.to_owned());
            self
        }
    }

    impl EnvSource for TestEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
    }

    fn parse(args: &[&str], env: &TestEnv) -> Result<StartupAction, CliError> {
        parse_from(args.iter().map(|arg| (*arg).to_owned()).collect(), env)
    }

    fn temp_pgdata(name: &str, conf: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "fastpg-server-cli-test-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("postgresql.conf"), conf).unwrap();
        dir
    }

    #[test]
    fn legacy_fastpg_positional_addr_still_works() {
        let action = parse(&["fastpg-server", "127.0.0.1:65432"], &TestEnv::default()).unwrap();

        assert_eq!(
            action,
            StartupAction::Serve(ServerConfig {
                addr: "127.0.0.1:65432".to_owned(),
                pgdata: None,
                pid_file: None,
            })
        );
    }

    #[test]
    fn postgres_mode_reads_config_and_cli_overrides_port() {
        let pgdata = temp_pgdata(
            "config-cli-precedence",
            "listen_addresses='127.0.0.1'\nport=6111\n",
        );
        let action = parse(
            &[
                "postgres",
                "-D",
                pgdata.to_str().unwrap(),
                "-c",
                "port=6222",
            ],
            &TestEnv::default(),
        )
        .unwrap();

        let StartupAction::Serve(config) = action else {
            panic!("expected serve action");
        };
        assert_eq!(config.addr, "127.0.0.1:6222");
        assert_eq!(config.pgdata, Some(pgdata.clone()));
        assert_eq!(config.pid_file.unwrap().path, pgdata.join("postmaster.pid"));
    }

    #[test]
    fn postgres_mode_uses_pgdata_env_before_fastpg_pgdata() {
        let pgdata = temp_pgdata("pgdata-env", "port=6333\n");
        let other = temp_pgdata("fastpg-pgdata-env", "port=6444\n");
        let env = TestEnv::default()
            .with("PGDATA", pgdata.to_str().unwrap())
            .with("FASTPG_PGDATA", other.to_str().unwrap());
        let action = parse(&["postgres"], &env).unwrap();

        let StartupAction::Serve(config) = action else {
            panic!("expected serve action");
        };
        assert_eq!(config.addr, "127.0.0.1:6333");
        assert_eq!(config.pgdata, Some(pgdata));
    }

    #[test]
    fn unix_socket_when_tcp_disabled() {
        let pgdata = temp_pgdata(
            "unix-socket",
            "listen_addresses=''\nport=6333\nunix_socket_directories='/tmp'\n",
        );
        let action = parse(
            &["postgres", "-D", pgdata.to_str().unwrap()],
            &TestEnv::default(),
        )
        .unwrap();

        let StartupAction::Serve(config) = action else {
            panic!("expected serve action");
        };
        assert_eq!(config.addr, "unix:/tmp/.s.PGSQL.6333");
    }

    #[test]
    fn unsupported_postgres_flag_is_rejected() {
        let error = parse(&["postgres", "-B", "128"], &TestEnv::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported PostgreSQL server option: -B")
        );
    }

    #[test]
    fn c_query_prints_resolved_setting() {
        let pgdata = temp_pgdata("query", "port=6111\n");
        let action = parse(
            &["postgres", "-D", pgdata.to_str().unwrap(), "-C", "port"],
            &TestEnv::default(),
        )
        .unwrap();

        assert_eq!(action, StartupAction::PrintAndExit("6111\n".to_owned()));
    }

    #[test]
    fn c_query_can_print_data_directory() {
        let pgdata = temp_pgdata("query-data-directory", "port=6111\n");
        let action = parse(
            &[
                "postgres",
                "-D",
                pgdata.to_str().unwrap(),
                "-C",
                "data_directory",
            ],
            &TestEnv::default(),
        )
        .unwrap();

        assert_eq!(
            action,
            StartupAction::PrintAndExit(format!("{}\n", pgdata.display()))
        );
    }

    #[test]
    fn c_assignment_rejects_data_directory() {
        let error = parse(
            &["postgres", "-c", "data_directory=/tmp/pgdata"],
            &TestEnv::default(),
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PostgreSQL configuration setting: data_directory"
        );
    }

    #[test]
    fn postgres_argv_version_matches_postgres_tools() {
        let action = parse(&["postgres", "--version"], &TestEnv::default()).unwrap();

        assert_eq!(
            action,
            StartupAction::PrintAndExit(format!(
                "postgres (PostgreSQL) {}\n",
                fastpg_compat::DEFAULT_SERVER_VERSION
            ))
        );
    }
}
