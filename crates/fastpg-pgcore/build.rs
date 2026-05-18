use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=c/pgcore_raw_parser.c");
    println!("cargo:rerun-if-env-changed=FASTPG_POSTGRES_BUILD_DIR");

    if env::var_os("CARGO_FEATURE_POSTGRES_LINKED").is_none() {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("fastpg-pgcore must live under <repo>/crates/fastpg-pgcore")
        .to_path_buf();
    let build_dir = env::var_os("FASTPG_POSTGRES_BUILD_DIR")
        .map(PathBuf::from)
        .expect("FASTPG_POSTGRES_BUILD_DIR must point at a Meson Postgres build directory");
    let build_dir = if build_dir.is_absolute() {
        build_dir
    } else {
        source_root.join(build_dir)
    };
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    compile_shim(&source_root, &build_dir);

    let archive = out_dir.join("libfastpg_pgcore_backend.a");
    build_backend_archive(&build_dir, &out_dir, &archive);

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=fastpg_pgcore_backend");
}

fn compile_shim(source_root: &Path, build_dir: &Path) {
    let target = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let mut build = cc::Build::new();
    build
        .file("c/pgcore_raw_parser.c")
        .include(source_root.join("src/include"))
        .include(source_root.join("src/include/port"))
        .include(source_root.join("src/backend"))
        .include(build_dir.join("src/include"))
        .warnings(false);

    let platform_include = source_root.join("src/include/port").join(&target);
    if platform_include.exists() {
        build.include(platform_include);
    }

    build.compile("fastpg_pgcore_raw_parser");
}

fn build_backend_archive(build_dir: &Path, out_dir: &Path, archive: &Path) {
    if archive.exists() {
        fs::remove_file(archive).unwrap_or_else(|error| {
            panic!("failed to remove stale {}: {error}", archive.display());
        });
    }

    let object_dir = out_dir.join("postgres-objects");
    if object_dir.exists() {
        fs::remove_dir_all(&object_dir).unwrap_or_else(|error| {
            panic!("failed to remove stale {}: {error}", object_dir.display());
        });
    }
    fs::create_dir_all(&object_dir).unwrap_or_else(|error| {
        panic!("failed to create {}: {error}", object_dir.display());
    });

    let mut objects = Vec::new();
    collect_backend_objects(build_dir, &mut objects);
    extract_archive_objects(
        build_dir,
        &build_dir.join("src/backend/parser/parser.a"),
        &object_dir.join("parser"),
        &mut objects,
    );
    extract_archive_objects(
        build_dir,
        &build_dir.join("src/common/libpgcommon_srv.a"),
        &object_dir.join("pgcommon_srv"),
        &mut objects,
    );
    extract_archive_objects(
        build_dir,
        &build_dir.join("src/common/libpgcommon_srv_config_info.a"),
        &object_dir.join("pgcommon_srv_config_info"),
        &mut objects,
    );
    extract_archive_objects(
        build_dir,
        &build_dir.join("src/common/libpgcommon_srv_ryu.a"),
        &object_dir.join("pgcommon_srv_ryu"),
        &mut objects,
    );
    extract_archive_objects(
        build_dir,
        &build_dir.join("src/port/libpgport_srv.a"),
        &object_dir.join("pgport_srv"),
        &mut objects,
    );

    if objects.is_empty() {
        panic!(
            "no Postgres backend objects found under {}",
            build_dir.display()
        );
    }

    run_command(
        Command::new(ar_program())
            .arg("crs")
            .arg(archive)
            .args(&objects),
        "build Postgres backend archive",
    );
}

fn collect_backend_objects(build_dir: &Path, objects: &mut Vec<PathBuf>) {
    for relative in [
        "src/backend/postgres_lib.a.p",
        "src/backend/nodes/nodefuncs.a.p",
        "src/backend/storage/page/checksum_backend_lib.a.p",
        "src/backend/utils/activity/wait_event_names.a.p",
        "src/backend/utils/adt/jsonpath.a.p",
        "src/backend/utils/adt/numeric_backend_lib.a.p",
        "src/backend/utils/misc/guc-file.a.p",
    ] {
        collect_object_dir(&build_dir.join(relative), objects);
    }
}

fn collect_object_dir(dir: &Path, objects: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }

    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).unwrap_or_else(|error| {
            panic!("failed to read {}: {error}", current.display());
        }) {
            let path = entry
                .unwrap_or_else(|error| {
                    panic!("failed to read entry in {}: {error}", current.display());
                })
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension() == Some(OsStr::new("o")) && should_link_backend_object(&path)
            {
                println!("cargo:rerun-if-changed={}", path.display());
                objects.push(path);
            }
        }
    }
}

fn should_link_backend_object(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    name != "main_main.c.o"
}

fn extract_archive_objects(
    build_dir: &Path,
    archive: &Path,
    output_dir: &Path,
    objects: &mut Vec<PathBuf>,
) {
    if !archive.exists() {
        panic!("missing required Postgres archive: {}", archive.display());
    }

    if is_thin_archive(archive) {
        for member in archive_members(archive) {
            if Path::new(&member).extension() != Some(OsStr::new("o")) {
                continue;
            }
            let path = resolve_thin_archive_member(build_dir, archive, &member);
            println!("cargo:rerun-if-changed={}", path.display());
            objects.push(path);
        }
        return;
    }

    fs::create_dir_all(output_dir).unwrap_or_else(|error| {
        panic!("failed to create {}: {error}", output_dir.display());
    });

    run_command(
        Command::new(ar_program())
            .arg("x")
            .arg(archive)
            .current_dir(output_dir),
        "extract Postgres archive",
    );

    for entry in fs::read_dir(output_dir).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", output_dir.display());
    }) {
        let path = entry
            .unwrap_or_else(|error| {
                panic!("failed to read entry in {}: {error}", output_dir.display());
            })
            .path();
        if path.extension() == Some(OsStr::new("o")) {
            objects.push(path);
        }
    }
}

fn is_thin_archive(archive: &Path) -> bool {
    let mut file = fs::File::open(archive).unwrap_or_else(|error| {
        panic!("failed to open {}: {error}", archive.display());
    });
    let mut header = [0; 8];
    let read = file.read(&mut header).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", archive.display());
    });
    read == header.len() && header == *b"!<thin>\n"
}

fn archive_members(archive: &Path) -> Vec<String> {
    let output = run_command_output(
        Command::new(ar_program()).arg("t").arg(archive),
        "list Postgres archive members",
    );
    String::from_utf8_lossy(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn resolve_thin_archive_member(build_dir: &Path, archive: &Path, member: &str) -> PathBuf {
    let member_path = Path::new(member);
    if member_path.is_absolute() {
        return member_path.to_path_buf();
    }

    let archive_dir = archive.parent().unwrap_or_else(|| Path::new("."));
    for candidate in [archive_dir.join(member_path), build_dir.join(member_path)] {
        if candidate.exists() {
            return candidate;
        }
    }

    panic!(
        "thin archive member {member:?} from {} was not found relative to {} or {}",
        archive.display(),
        archive_dir.display(),
        build_dir.display()
    );
}

fn ar_program() -> String {
    env::var("AR").unwrap_or_else(|_| "ar".to_owned())
}

fn run_command(command: &mut Command, label: &str) {
    let _ = run_command_output(command, label);
}

fn run_command_output(command: &mut Command, label: &str) -> Vec<u8> {
    let output = command.output().unwrap_or_else(|error| {
        panic!("failed to {label}: {error}");
    });
    if !output.status.success() {
        panic!(
            "failed to {label}: status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output.stdout
}
