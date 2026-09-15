mod app;
mod markdown;
mod nvim;
mod tikz;
mod vault;

fn main() {
    app::run(std::env::args_os().nth(1).map(Into::into));
}
