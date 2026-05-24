use std::io;
use std::net::SocketAddr;

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
static POSTGRES_CATALOG_BOOTSTRAP: OnceLock<Result<CatalogBootstrap, String>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestServerConfig {
    pub addr: String,
}

#[derive(Debug)]
pub struct TestServer {
    addr: SocketAddr,
    server_task: JoinHandle<io::Result<()>>,
}

impl TestServer {
    pub async fn start() -> io::Result<Self> {
        ensure_postgres_catalog_bootstrap()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server_task = tokio::spawn(fastpg_server::serve_listener(listener));

        Ok(Self { addr, server_task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn connection_string(&self) -> String {
        format!("postgres://{}/postgres", self.addr)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

#[derive(Debug)]
#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
struct CatalogBootstrap {
    _pgdata: PathBuf,
    _pgcore_libdir: PathBuf,
}

#[cfg(any(not(feature = "postgres-execution"), feature = "rust-catalog"))]
fn ensure_postgres_catalog_bootstrap() -> io::Result<()> {
    Ok(())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn ensure_postgres_catalog_bootstrap() -> io::Result<()> {
    match POSTGRES_CATALOG_BOOTSTRAP.get_or_init(bootstrap_postgres_catalog) {
        Ok(_) => Ok(()),
        Err(message) => Err(io::Error::other(message.clone())),
    }
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn bootstrap_postgres_catalog() -> Result<CatalogBootstrap, String> {
    if let Some(pgdata) = existing_pgdata()? {
        return Ok(CatalogBootstrap {
            _pgdata: pgdata,
            _pgcore_libdir: PathBuf::new(),
        });
    }

    let build_dir = required_absolute_dir("FASTPG_POSTGRES_BUILD_DIR")?;
    let client_bindir = postgres_client_bindir(&build_dir)?;
    let client_libdir = client_bindir
        .parent()
        .map(|prefix| prefix.join("lib"))
        .ok_or_else(|| {
            format!(
                "could not derive PostgreSQL libdir from client bindir {}",
                client_bindir.display()
            )
        })?;
    repair_macos_client_library_names(&client_bindir, &client_libdir)?;
    let pgcore_extension_libdir = pgcore_extension_libdir(&build_dir)?;
    let pgcore_libdir = prepare_pgcore_libdir(&build_dir, pgcore_extension_libdir.as_deref())?;
    let pgdata = create_catalog_pgdata(&client_bindir, &client_libdir)?;

    let postgres_exec = client_bindir.join(exe_name("postgres"));
    set_env_var("FASTPG_EXEC_PATH", postgres_exec);
    set_env_var("FASTPG_PGLIBDIR", &pgcore_libdir);
    prepend_env_path(library_path_var(), &pgcore_libdir);
    set_env_var("FASTPG_PGDATA", &pgdata);

    Ok(CatalogBootstrap {
        _pgdata: pgdata,
        _pgcore_libdir: pgcore_libdir,
    })
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn existing_pgdata() -> Result<Option<PathBuf>, String> {
    let Some(pgdata) = env::var_os("FASTPG_PGDATA") else {
        return Ok(None);
    };
    if pgdata.is_empty() {
        return Ok(None);
    }
    let pgdata = PathBuf::from(pgdata);
    if !pgdata.is_absolute() {
        return Err(format!(
            "FASTPG_PGDATA must be absolute for postgres catalog tests: {}",
            pgdata.display()
        ));
    }
    if !pgdata.is_dir() {
        return Err(format!(
            "FASTPG_PGDATA does not exist or is not a directory: {}",
            pgdata.display()
        ));
    }
    Ok(Some(pgdata))
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn required_absolute_dir(name: &str) -> Result<PathBuf, String> {
    let value = env::var_os(name)
        .ok_or_else(|| format!("{name} is required to bootstrap postgres catalog tests"))?;
    let path = PathBuf::from(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        resolve_relative_path(&path)?
    };
    let absolute = absolute.canonicalize().map_err(|error| {
        format!(
            "could not canonicalize {name}={}: {error}",
            absolute.display()
        )
    })?;
    if !absolute.is_dir() {
        return Err(format!("{name} is not a directory: {}", absolute.display()));
    }
    Ok(absolute)
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn resolve_relative_path(path: &Path) -> Result<PathBuf, String> {
    let cwd_candidate = env::current_dir()
        .map_err(|error| format!("could not read current directory: {error}"))?
        .join(path);
    if cwd_candidate.exists() {
        return Ok(cwd_candidate);
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_candidate = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(|workspace| workspace.join(path));
    if let Some(workspace_candidate) = workspace_candidate
        && workspace_candidate.exists()
    {
        return Ok(workspace_candidate);
    }

    Ok(cwd_candidate)
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn postgres_client_bindir(build_dir: &Path) -> Result<PathBuf, String> {
    if let Some(value) = env::var_os("FASTPG_POSTGRES_CLIENT_BINDIR") {
        let bindir = PathBuf::from(value);
        if bindir.join(exe_name("initdb")).is_file() {
            return Ok(bindir);
        }
        return Err(format!(
            "FASTPG_POSTGRES_CLIENT_BINDIR does not contain initdb: {}",
            bindir.display()
        ));
    }

    let mut candidates = Vec::new();
    if let Some(parent) = build_dir.parent() {
        candidates.push(parent.join("normal/tmp_install/usr/local/pgsql/bin"));
    }
    candidates.push(build_dir.join("tmp_install/usr/local/pgsql/bin"));

    candidates
        .into_iter()
        .find(|candidate| candidate.join(exe_name("initdb")).is_file())
        .ok_or_else(|| {
            format!(
                "could not find initdb for postgres catalog tests; set FASTPG_POSTGRES_CLIENT_BINDIR or build the pgbench harness next to {}",
                build_dir.display()
            )
        })
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn pgcore_extension_libdir(build_dir: &Path) -> Result<Option<PathBuf>, String> {
    if let Some(value) = env::var_os("FASTPG_POSTGRES_EXTENSION_LIBDIR") {
        let libdir = PathBuf::from(value);
        if !libdir.is_dir() {
            return Err(format!(
                "FASTPG_POSTGRES_EXTENSION_LIBDIR does not exist or is not a directory: {}",
                libdir.display()
            ));
        }
        return Ok(Some(libdir));
    }

    let libdir = build_dir.join("tmp_install/usr/local/pgsql/lib");
    if installed_pgvector_libraries(&libdir)?.is_empty() {
        Ok(None)
    } else {
        Ok(Some(libdir))
    }
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn prepare_pgcore_libdir(
    build_dir: &Path,
    extension_libdir: Option<&Path>,
) -> Result<PathBuf, String> {
    let libdir = build_dir.join("fastpg-pgcore-libdir");
    fs::create_dir_all(&libdir).map_err(|error| {
        format!(
            "could not create pgcore library directory {}: {error}",
            libdir.display()
        )
    })?;

    let suffix = dylib_suffix();
    let mut candidates = vec![
        build_dir.join(format!("src/pl/plpgsql/src/plpgsql{suffix}")),
        build_dir.join(format!("src/backend/snowball/dict_snowball{suffix}")),
        build_dir.join(format!(
            "src/backend/replication/libpqwalreceiver/libpqwalreceiver{suffix}"
        )),
        build_dir.join(format!("src/interfaces/libpq/libpq.5{suffix}")),
        build_dir.join(format!("src/interfaces/libpq/libpq{suffix}")),
    ];

    let conversion_dir = build_dir.join("src/backend/utils/mb/conversion_procs");
    if let Ok(entries) = fs::read_dir(&conversion_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(suffix))
            {
                candidates.push(path);
            }
        }
    }

    for source in candidates {
        if !source.is_file() {
            continue;
        }
        copy_pgcore_library(&source, &libdir)?;
    }

    remove_pgcore_pgvector_libraries(&libdir)?;
    if let Some(extension_libdir) = extension_libdir {
        for source in installed_pgvector_libraries(extension_libdir)? {
            copy_pgcore_library(&source, &libdir)?;
        }
    }

    Ok(libdir)
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn remove_pgcore_pgvector_libraries(libdir: &Path) -> Result<(), String> {
    for path in installed_pgvector_libraries(libdir)? {
        fs::remove_file(&path).map_err(|error| {
            format!(
                "could not remove stale pgvector library {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn copy_pgcore_library(source: &Path, libdir: &Path) -> Result<(), String> {
    let dest = libdir.join(source.file_name().expect("candidate has file name"));
    if dest.exists() {
        fs::remove_file(&dest).map_err(|error| {
            format!(
                "could not replace pgcore library {}: {error}",
                dest.display()
            )
        })?;
    }
    fs::copy(source, &dest).map_err(|error| {
        format!(
            "could not copy pgcore library {} to {}: {error}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn installed_pgvector_libraries(libdir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut libraries = Vec::new();
    collect_installed_pgvector_libraries(libdir, &mut libraries)?;
    libraries.sort();
    Ok(libraries)
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn collect_installed_pgvector_libraries(
    dir: &Path,
    libraries: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in
        fs::read_dir(dir).map_err(|error| format!("could not read {}: {error}", dir.display()))?
    {
        let path = entry
            .map_err(|error| format!("could not read entry in {}: {error}", dir.display()))?
            .path();
        if path.is_dir() {
            collect_installed_pgvector_libraries(&path, libraries)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_pgvector_library)
        {
            libraries.push(path);
        }
    }
    Ok(())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn is_pgvector_library(name: &str) -> bool {
    name.starts_with("vector")
        && (name.ends_with(".so") || name.contains(".so.") || name.ends_with(".dylib"))
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn repair_macos_client_library_names(
    client_bindir: &Path,
    client_libdir: &Path,
) -> Result<(), String> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }

    let old_name = "/usr/local/pgsql/lib/libpq.5.dylib";
    let new_name = client_libdir.join("libpq.5.dylib");
    let mut targets = vec![
        client_bindir.join("initdb"),
        client_bindir.join("postgres"),
        client_bindir.join("pg_ctl"),
        client_libdir.join("libpq.5.dylib"),
    ];
    if let Ok(entries) = fs::read_dir(client_libdir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".dylib"))
            {
                targets.push(path);
            }
        }
    }

    for target in targets {
        if !target.is_file() {
            continue;
        }
        let output = Command::new("otool")
            .arg("-L")
            .arg(&target)
            .output()
            .map_err(|error| format!("failed to inspect {}: {error}", target.display()))?;
        if !output.status.success() {
            return Err(format!(
                "otool -L failed for {} with status {}: {}",
                target.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let linked_libraries = String::from_utf8_lossy(&output.stdout);
        if target.file_name().and_then(|name| name.to_str()) == Some("libpq.5.dylib")
            && linked_libraries.contains(old_name)
        {
            run_install_name_tool(&target, &[OsStr::new("-id"), new_name.as_os_str()])?;
        } else if linked_libraries.contains(old_name) {
            run_install_name_tool(
                &target,
                &[
                    OsStr::new("-change"),
                    OsStr::new(old_name),
                    new_name.as_os_str(),
                ],
            )?;
        }
    }

    Ok(())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn run_install_name_tool(target: &Path, args: &[&OsStr]) -> Result<(), String> {
    let output = Command::new("install_name_tool")
        .args(args)
        .arg(target)
        .output()
        .map_err(|error| {
            format!(
                "failed to update install names for {}: {error}",
                target.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "install_name_tool failed for {} with status {}: {}",
            target.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn create_catalog_pgdata(client_bindir: &Path, client_libdir: &Path) -> Result<PathBuf, String> {
    let pgdata = unique_temp_dir("fastpg-testkit-pgdata")?;
    let initdb = client_bindir.join(exe_name("initdb"));
    let mut command = Command::new(&initdb);
    command
        .arg("-D")
        .arg(&pgdata)
        .arg("-U")
        .arg("postgres")
        .arg("-A")
        .arg("trust")
        .arg("--no-locale");

    command.env(
        "PATH",
        prepend_path_value(client_bindir, env::var_os("PATH")),
    );
    command.env(
        library_path_var(),
        prepend_path_value(client_libdir, env::var_os(library_path_var())),
    );

    let output = command.output().map_err(|error| {
        let _ = fs::remove_dir_all(&pgdata);
        format!("failed to run {}: {error}", initdb.display())
    })?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = fs::remove_dir_all(&pgdata);
        return Err(format!(
            "initdb failed for postgres catalog tests with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status, stdout, stderr
        ));
    }

    Ok(pgdata)
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn unique_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let path = env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir(&path).map_err(|error| {
        format!(
            "could not create temporary directory {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn dylib_suffix() -> &'static str {
    if cfg!(target_os = "macos") {
        ".dylib"
    } else if cfg!(windows) {
        ".dll"
    } else {
        ".so"
    }
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn library_path_var() -> &'static str {
    if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    }
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn prepend_env_path(name: &str, path: &Path) {
    set_env_var(name, prepend_path_value(path, env::var_os(name)));
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn prepend_path_value(path: &Path, existing: Option<OsString>) -> OsString {
    let mut paths = vec![path.to_path_buf()];
    if let Some(existing) = existing {
        paths.extend(env::split_paths(&existing));
    }
    env::join_paths(paths).unwrap_or_else(|_| path.as_os_str().to_os_string())
}

#[cfg(all(feature = "postgres-execution", not(feature = "rust-catalog")))]
fn set_env_var(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: this testkit bootstrap runs once, before the in-process FastPG
    // listener is started and before pgcore reads these variables. Other
    // callers block on the same OnceLock, so they observe a fully initialized
    // process environment.
    unsafe {
        env::set_var(key, value);
    }
}
