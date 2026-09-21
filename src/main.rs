mod app;
mod markdown;
mod math;
mod nvim;
mod tikz;
mod update;
mod vault;

fn main() {
    app::run(std::env::args_os().nth(1).map(Into::into));
}
