//! Exploration only: what MXC reports about this host.
fn main() {
    let support = mxc_sdk::v1::platform_support();
    println!("{support:#?}");
    for backend in mxc_sdk::v1::available_backends() {
        println!("{backend:#?}");
    }
}
