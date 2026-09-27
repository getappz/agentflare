fn main() {
    #[cfg(all(feature = "sqlcipher-bundled", feature = "sqlcipher-external"))]
    panic!("flare-db: choose either sqlcipher-bundled or sqlcipher-external, not both");

    #[cfg(feature = "sqlcipher-bundled")]
    bundled();
}

#[cfg(feature = "sqlcipher-bundled")]
fn bundled() {
    println!("cargo:rerun-if-changed=vendor/sqlcipher/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlcipher/sqlite3.h");
    println!("cargo:rerun-if-env-changed=OPENSSL_SRC_PERL");
    println!("cargo:rerun-if-env-changed=PERL");
    let crypto = openssl_src::Build::new().build();
    cc::Build::new()
        .file("vendor/sqlcipher/sqlite3.c")
        .include(crypto.include_dir())
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
    crypto.print_cargo_metadata();
}
