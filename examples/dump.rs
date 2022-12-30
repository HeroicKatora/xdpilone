fn main() {
    let args = <Args as clap::Parser>::parse();
}

#[derive(clap::Parser)]
struct Args {
}
