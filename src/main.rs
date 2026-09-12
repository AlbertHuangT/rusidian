mod app;
mod markdown;
mod tikz;

fn main() {
    app::run(std::env::args_os().nth(1).map(Into::into));
}
