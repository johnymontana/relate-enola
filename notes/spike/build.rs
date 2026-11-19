fn main() {
    println!("cargo:rerun-if-env-changed=ENOLA_BIN");
    let p = std::env::var("ENOLA_BIN").expect("set ENOLA_BIN to the verified enola binary");
    println!("cargo:rerun-if-changed={p}");
}
