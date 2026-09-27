fn main() {
    let modes = [
        cfg!(feature = "sqlcipher-bundled"),
        cfg!(feature = "sqlcipher-external"),
        cfg!(feature = "sqlcipher-bundled-external-openssl"),
    ];
    assert!(
        modes.into_iter().filter(|enabled| *enabled).count() <= 1,
        "flare-db: choose exactly one SQLCipher mode"
    );

    #[cfg(any(
        feature = "sqlcipher-bundled",
        feature = "sqlcipher-bundled-external-openssl"
    ))]
    bundled();
}

#[cfg(any(
    feature = "sqlcipher-bundled",
    feature = "sqlcipher-bundled-external-openssl"
))]
fn bundled() {
    println!("cargo:rerun-if-changed=vendor/sqlcipher/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlcipher/sqlite3.h");
    println!("cargo:rerun-if-env-changed=OPENSSL_SRC_PERL");
    println!("cargo:rerun-if-env-changed=PERL");
    #[cfg(feature = "sqlcipher-bundled")]
    let crypto = openssl_src::Build::new().build();
    #[cfg(feature = "sqlcipher-bundled")]
    let include = crypto.include_dir();
    #[cfg(all(
        feature = "sqlcipher-bundled-external-openssl",
        not(feature = "sqlcipher-bundled")
    ))]
    let (include, lib_dir, lib_name) = external_openssl();
    cc::Build::new()
        .file("vendor/sqlcipher/sqlite3.c")
        .include(include)
        .define("SQLITE_HAS_CODEC", "1")
        .define("SQLITE_TEMP_STORE", "2")
        .define("SQLITE_EXTRA_INIT", "sqlcipher_extra_init")
        .define("SQLITE_EXTRA_SHUTDOWN", "sqlcipher_extra_shutdown")
        .define("SQLITE_THREADSAFE", "1")
        .define("SQLITE_ENABLE_COLUMN_METADATA", "1")
        .define("SQLITE_ENABLE_UNLOCK_NOTIFY", "1")
        .define("SQLITE_ENABLE_FTS5", "1")
        .warnings(false)
        .compile("sqlcipher");
    #[cfg(feature = "sqlcipher-bundled")]
    crypto.print_cargo_metadata();
    #[cfg(all(
        feature = "sqlcipher-bundled-external-openssl",
        not(feature = "sqlcipher-bundled")
    ))]
    {
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        println!("cargo:rustc-link-lib=dylib={lib_name}");
    }
}

#[cfg(all(
    feature = "sqlcipher-bundled-external-openssl",
    not(feature = "sqlcipher-bundled")
))]
fn external_openssl() -> (std::path::PathBuf, std::path::PathBuf, &'static str) {
    use std::{env, path::PathBuf};
    for name in [
        "OPENSSL_DIR",
        "OPENSSL_INCLUDE_DIR",
        "OPENSSL_LIB_DIR",
        "OPENSSL_STATIC",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    assert!(
        env::var_os("OPENSSL_STATIC").is_none_or(|value| value == "0"),
        "flare-db: sqlcipher-bundled-external-openssl requires dynamic OpenSSL; unset OPENSSL_STATIC or set it to 0"
    );
    let root = env::var_os("OPENSSL_DIR").map(PathBuf::from);
    let include = env::var_os("OPENSSL_INCLUDE_DIR")
        .map(PathBuf::from)
        .or_else(|| root.as_ref().map(|path| path.join("include")))
        .expect("flare-db: set OPENSSL_INCLUDE_DIR or OPENSSL_DIR to the prebuilt OpenSSL SDK");
    let lib_dir = env::var_os("OPENSSL_LIB_DIR")
        .map(PathBuf::from)
        .or_else(|| root.as_ref().map(|path| path.join("lib")))
        .expect("flare-db: set OPENSSL_LIB_DIR or OPENSSL_DIR to the prebuilt OpenSSL SDK");
    let (lib_name, lib_file) = match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") => {
            ("libcrypto", "libcrypto.lib")
        }
        Ok("linux") => ("crypto", "libcrypto.so"),
        _ => panic!("flare-db: external OpenSSL bundling supports Windows MSVC and Linux"),
    };
    assert!(
        include.is_absolute() && lib_dir.is_absolute(),
        "flare-db: OpenSSL SDK paths must be absolute"
    );
    assert!(
        include.join("openssl/crypto.h").is_file(),
        "flare-db: OPENSSL_INCLUDE_DIR must contain openssl/crypto.h"
    );
    assert!(
        lib_dir.join(lib_file).is_file(),
        "flare-db: OPENSSL_LIB_DIR must contain {lib_file}"
    );
    println!("cargo:rerun-if-changed={}", include.display());
    println!(
        "cargo:rerun-if-changed={}",
        lib_dir.join(lib_file).display()
    );
    (include, lib_dir, lib_name)
}
