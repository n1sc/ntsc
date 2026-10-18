use std::path::{Path, PathBuf};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[test]
fn shared_layouts_and_borrowed_receivers_survive_cross_adaptor_calls() {
    let root =
        Fixture(std::env::temp_dir().join(format!("ntsc_shared_layouts_{}", std::process::id())));
    let registry = root.0.join("registry");
    write(
        &registry,
        "owner-1.0.0/Cargo.toml",
        "[package]\nname = \"owner\"\nversion = \"1.0.0\"\nedition = \"2021\"\n[features]\nwide = []\n",
    );
    write(
        &registry,
        "owner-1.0.0/src/lib.rs",
        r#"
        pub struct Builder {
            #[cfg(feature = "wide")]
            padding: [u64; 8],
            label: String,
        }
        impl Builder {
            pub fn new(label: String) -> Self {
                Self { #[cfg(feature = "wide")] padding: [123; 8], label }
            }
            pub fn append(&mut self, text: String) { self.label.push_str(&text); }
            pub fn label(&self) -> &str { &self.label }
            pub fn inspect(&self, other: Builder) -> String { format!("{}:{}", self.label, other.label) }
            pub fn fail(&mut self) { self.label.push('!'); panic!("fixture panic"); }
        }
    "#,
    );
    write(
        &registry,
        "consumer-1.0.0/Cargo.toml",
        "[package]\nname = \"consumer\"\nversion = \"1.0.0\"\nedition = \"2021\"\n[dependencies]\nowner = { version = \"1\", features = [\"wide\"] }\n",
    );
    write(
        &registry,
        "consumer-1.0.0/src/lib.rs",
        "pub fn attach(mut builder: owner::Builder) -> owner::Builder { builder.append(String::from(\" attached\")); builder }\n",
    );
    write(
        &root.0,
        "neutron.toml",
        "[rust-deps]\nowner = \"1\"\nconsumer = \"1\"\n",
    );
    write(
        &root.0,
        "ntsc.lock",
        "[[package]]\nname = \"owner\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"fixture\"\n[[package]]\nname = \"consumer\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"fixture\"\n",
    );
    let cache = ntsc_cargo::cache::CacheRoot::at(root.0.join("cache"));
    for name in ["owner", "consumer"] {
        ntsc_cargo::adaptor::emit_adaptor(&cache, &root.0, name, "1.0.0", &registry, None, None)
            .unwrap();
    }
    let artifacts =
        ntsc_cargo::build::build_adaptors_with_native_links(&cache, &root.0, false).unwrap();
    assert_eq!(artifacts.archives.len(), 1);
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    assert!(
        std::process::Command::new("cargo")
            .args(["build", "--offline", "-j", "2", "-p", "ntsc-runtime"])
            .current_dir(workspace)
            .status()
            .unwrap()
            .success()
    );
    ntsc_codegen::emit::set_rust_export_types(&root.0);
    let object = ntsc_codegen::compile_source(
        r#"
        use @owner
        use @consumer
        use sys
        async fun use_builder(int builder) {
            await async.sleep(50)
            say(@owner.Builder_label(builder))
        }
        fun spawn_worker() {
            var int worker = @owner.Builder_new("worker")
            go use_builder(worker)
        }
        fun main() -> int {
            var int builder = @owner.Builder_new("start")
            builder = @consumer.attach(builder)
            @owner.Builder_append(builder, " updated")
            say(@owner.Builder_inspect(builder, @owner.Builder_new("other")))
            try { @owner.Builder_fail(builder) } catch (err) { say("caught") }
            say(@owner.Builder_label(builder))
            spawn_worker()
            sys.sleep(200)
            return 0
        }
    "#,
        ntsc_codegen::host_triple(),
        "borrows",
        &root.0,
    )
    .unwrap();
    let binary = root.0.join("borrows");
    ntsc_codegen::link_binary_with_extras(
        &object,
        &workspace
            .join("target/debug")
            .join(ntsc_codegen::runtime_lib_name()),
        &artifacts.archives,
        &artifacts.native_link_args,
        &binary,
    )
    .unwrap();
    let output = std::process::Command::new(binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "start attached updated:other\ncaught\nstart attached updated!\nworker\n"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("memory leak detected"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cow_parameters_are_generated_without_a_wrapper_crate() {
    let root =
        Fixture(std::env::temp_dir().join(format!("ntsc_cow_adaptor_{}", std::process::id())));
    let registry = root.0.join("registry");
    write(
        &registry,
        "cow-api-1.0.0/Cargo.toml",
        "[package]\nname = \"cow-api\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
    );
    write(
        &registry,
        "cow-api-1.0.0/src/lib.rs",
        r#"
        use std::borrow::Cow;

        pub fn text(value: Cow<'_, str>) -> Cow<'_, str> { value }
        pub fn bytes(value: Cow<'_, [u8]>) -> Cow<'_, [u8]> { value }
        "#,
    );
    write(&root.0, "neutron.toml", "[rust-deps]\ncow-api = \"1\"\n");
    write(
        &root.0,
        "ntsc.lock",
        "[[package]]\nname = \"cow-api\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"fixture\"\n",
    );

    let cache = ntsc_cargo::cache::CacheRoot::at(root.0.join("cache"));
    ntsc_cargo::adaptor::emit_adaptor(&cache, &root.0, "cow-api", "1.0.0", &registry, None, None)
        .unwrap();
    let artifacts =
        ntsc_cargo::build::build_adaptors_with_native_links(&cache, &root.0, false).unwrap();
    assert_eq!(artifacts.archives.len(), 1);

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    assert!(
        std::process::Command::new("cargo")
            .args(["build", "--offline", "-j", "2", "-p", "ntsc-runtime"])
            .current_dir(workspace)
            .status()
            .unwrap()
            .success()
    );
    ntsc_codegen::emit::set_rust_export_types(&root.0);
    let object = ntsc_codegen::compile_source(
        r#"
        use @cow_api
        fun main() -> int {
            say(@cow_api.text("hello"))
            say(@cow_api.bytes("bytes"))
            return 0
        }
        "#,
        ntsc_codegen::host_triple(),
        "cow_api",
        &root.0,
    )
    .unwrap();
    let binary = root.0.join("cow_api");
    ntsc_codegen::link_binary_with_extras(
        &object,
        &workspace
            .join("target/debug")
            .join(ntsc_codegen::runtime_lib_name()),
        &artifacts.archives,
        &artifacts.native_link_args,
        &binary,
    )
    .unwrap();
    let output = std::process::Command::new(binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\nbytes\n");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("memory leak detected"));
}
