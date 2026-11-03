use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const EMBEDDED_BROKER_PATH_ENV: &str = "LCR_EMBEDDED_BROKER_PATH";
const EMBEDDED_BROKER_MANIFEST_PATH_ENV: &str = "LCR_EMBEDDED_BROKER_MANIFEST_PATH";
const EMBEDDED_BROKER_RESOURCE_ID: u16 = 101;
const EMBEDDED_BROKER_MANIFEST_RESOURCE_ID: u16 = 102;

struct ResourceSpec<'a> {
    template_name: &'a str,
    generated_name: &'a str,
    resource_name: &'a str,
    binary_name: &'a str,
}

struct ResourceValues<'a> {
    icon: &'a str,
    numeric: &'a str,
    version: &'a str,
    broker_payload_resource: &'a str,
    broker_manifest_resource: &'a str,
}

fn cargo_version_component(name: &str) -> u16 {
    let value = env::var(name).unwrap_or_else(|_| panic!("{name} is required"));

    value
        .parse::<u16>()
        .unwrap_or_else(|_| panic!("{name} must fit a Windows VERSIONINFO u16 component"))
}

fn rc_string(value: &str, label: &str) -> String {
    if value.contains(['"', '\0']) {
        panic!("{label} contains a character unsafe for VERSIONINFO");
    }

    value.to_owned()
}

fn compile_resource(
    manifest_dir: &Path,
    out: &Path,
    spec: ResourceSpec<'_>,
    values: ResourceValues<'_>,
) {
    let template = fs::read_to_string(manifest_dir.join(spec.template_name))
        .unwrap_or_else(|_| panic!("{} template is required", spec.template_name));

    let generated = template
        .replace("@ICON_PATH@", values.icon)
        .replace("@VERSION_NUMERIC@", values.numeric)
        .replace("@VERSION_STRING@", values.version)
        .replace("@BROKER_PAYLOAD_RESOURCE@", values.broker_payload_resource)
        .replace(
            "@BROKER_SIGNED_MANIFEST_RESOURCE@",
            values.broker_manifest_resource,
        );

    if generated.contains("@ICON_PATH@")
        || generated.contains("@VERSION_NUMERIC@")
        || generated.contains("@VERSION_STRING@")
        || generated.contains("@BROKER_PAYLOAD_RESOURCE@")
        || generated.contains("@BROKER_SIGNED_MANIFEST_RESOURCE@")
    {
        panic!("unresolved resource placeholder in {}", spec.template_name);
    }

    let generated_rc = out.join(spec.generated_name);
    let resource = out.join(spec.resource_name);

    fs::write(&generated_rc, generated)
        .unwrap_or_else(|_| panic!("failed to write {}", spec.generated_name));

    let status = Command::new("llvm-rc")
        .args(["/fo"])
        .arg(&resource)
        .arg(&generated_rc)
        .status()
        .unwrap_or_else(|_| panic!("llvm-rc is required for {}", spec.template_name));

    assert!(
        status.success(),
        "llvm-rc failed for {}",
        spec.template_name
    );

    println!(
        "cargo:rustc-link-arg-bin={}={}",
        spec.binary_name,
        resource.display()
    );
}

fn embedded_broker_resource_line() -> String {
    let Some(raw_path) = env::var_os(EMBEDDED_BROKER_PATH_ENV) else {
        return String::new();
    };

    let path = PathBuf::from(raw_path);

    if !path.is_absolute() {
        panic!("{EMBEDDED_BROKER_PATH_ENV} must be an absolute path");
    }

    let metadata = fs::metadata(&path)
        .unwrap_or_else(|_| panic!("{EMBEDDED_BROKER_PATH_ENV} must identify an existing file"));

    if !metadata.is_file() || metadata.len() == 0 {
        panic!("{EMBEDDED_BROKER_PATH_ENV} must identify a non-empty file");
    }

    let path_text = path
        .to_str()
        .unwrap_or_else(|| panic!("{EMBEDDED_BROKER_PATH_ENV} must be Unicode"))
        .replace('\\', "/");

    let path_text = rc_string(&path_text, EMBEDDED_BROKER_PATH_ENV);

    println!("cargo:rerun-if-changed={}", path.display());

    format!("{EMBEDDED_BROKER_RESOURCE_ID} RCDATA \"{path_text}\"")
}

fn embedded_broker_manifest_resource_line() -> String {
    let Some(raw_path) = env::var_os(EMBEDDED_BROKER_MANIFEST_PATH_ENV) else {
        return String::new();
    };

    let path = PathBuf::from(raw_path);

    if !path.is_absolute() {
        panic!("{EMBEDDED_BROKER_MANIFEST_PATH_ENV} must be an absolute path");
    }

    let metadata = fs::metadata(&path).unwrap_or_else(|_| {
        panic!("{EMBEDDED_BROKER_MANIFEST_PATH_ENV} must identify an existing file")
    });

    if !metadata.is_file() || metadata.len() == 0 {
        panic!("{EMBEDDED_BROKER_MANIFEST_PATH_ENV} must identify a non-empty file");
    }

    let path_text = path
        .to_str()
        .unwrap_or_else(|| panic!("{EMBEDDED_BROKER_MANIFEST_PATH_ENV} must be Unicode"))
        .replace('\\', "/");

    let path_text = rc_string(&path_text, EMBEDDED_BROKER_MANIFEST_PATH_ENV);

    println!("cargo:rerun-if-changed={}", path.display());

    format!("{EMBEDDED_BROKER_MANIFEST_RESOURCE_ID} RCDATA \"{path_text}\"")
}
fn main() {
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    let version = rc_string(
        &env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"),
        "CARGO_PKG_VERSION",
    );

    let major = cargo_version_component("CARGO_PKG_VERSION_MAJOR");
    let minor = cargo_version_component("CARGO_PKG_VERSION_MINOR");
    let patch = cargo_version_component("CARGO_PKG_VERSION_PATCH");
    let numeric = format!("{major},{minor},{patch},0");

    let icon = manifest_dir
        .join("assets")
        .join("LCR_Helper.ico")
        .to_string_lossy()
        .replace('\\', "/");

    let icon = rc_string(&icon, "icon path");
    let broker_payload_resource = embedded_broker_resource_line();
    let broker_manifest_resource = embedded_broker_manifest_resource_line();

    compile_resource(
        &manifest_dir,
        &out,
        ResourceSpec {
            template_name: "lcr_helper.rc",
            generated_name: "lcr_helper.generated.rc",
            resource_name: "lcr_helper.res",
            binary_name: "LCR",
        },
        ResourceValues {
            icon: &icon,
            numeric: &numeric,
            version: &version,
            broker_payload_resource: &broker_payload_resource,
            broker_manifest_resource: &broker_manifest_resource,
        },
    );

    compile_resource(
        &manifest_dir,
        &out,
        ResourceSpec {
            template_name: "lcr_input_broker.rc",
            generated_name: "lcr_input_broker.generated.rc",
            resource_name: "lcr_input_broker.res",
            binary_name: "LCRInputBroker",
        },
        ResourceValues {
            icon: &icon,
            numeric: &numeric,
            version: &version,
            broker_payload_resource: "",
            broker_manifest_resource: "",
        },
    );

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=lcr_helper.rc");
    println!("cargo:rerun-if-changed=lcr_input_broker.rc");
    println!("cargo:rerun-if-changed=assets/LCR_Helper.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION_MAJOR");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION_MINOR");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION_PATCH");
    println!("cargo:rerun-if-env-changed={EMBEDDED_BROKER_PATH_ENV}");
    println!("cargo:rerun-if-env-changed={EMBEDDED_BROKER_MANIFEST_PATH_ENV}");
}
