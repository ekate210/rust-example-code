fn main() {
    cxx_build::bridge("src/lib.rs")
        .file("cpp/geo_distance.cc")
        .std("c++17")
        .compile("geo_distance_ffi");

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cpp/geo_distance.cc");
    println!("cargo:rerun-if-changed=include/geo_distance.h");
}
