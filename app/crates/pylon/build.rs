fn main() {
    cxx_build::bridge("src/pylon.rs")
        .file("cpp/pylon_bridge.cpp")
        .include("/opt/pylon/include")
        .flag("-std=c++17")
        .compile("pylon-bridge");

    println!("cargo:rustc-link-search=native=/opt/pylon/lib");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/opt/pylon/lib");
    println!("cargo:rustc-link-lib=pylonbase");
    println!("cargo:rustc-link-lib=pylonutility");
    println!("cargo:rustc-link-lib=GenApi_gcc_v3_5_Basler_pylon_v1");
    println!("cargo:rustc-link-lib=GCBase_gcc_v3_5_Basler_pylon_v1");
    println!("cargo:rerun-if-changed=cpp/pylon_bridge.h");
    println!("cargo:rerun-if-changed=cpp/pylon_bridge.cpp");
    println!("cargo:rerun-if-changed=src/pylon.rs");
}
