// Compile probe: proves gpui + gpui-kit resolve and build on this machine.
// Intentionally does not reference either crate yet — a declared dependency is
// still compiled, and touching APIs would couple the probe to API churn.

fn main() {
    println!("gpui probe");
}
