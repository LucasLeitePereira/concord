fn main() {
    println!(
        "sala-core {} (protocol v{})",
        env!("CARGO_PKG_VERSION"),
        sala_protocol::PROTOCOL_VERSION
    );
}
