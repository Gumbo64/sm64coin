fn main() {
    // SM64COIN_VERSION (the release tag, set by the release workflow) is compiled into the
    // binaries for the "update available" notice; unset = "dev" = no check.
    println!("cargo:rerun-if-env-changed=SM64COIN_VERSION");
}
