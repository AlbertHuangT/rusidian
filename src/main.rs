mod app;
mod fonts;
mod markdown;
mod math;
mod nvim;
mod paths;
mod settings;
mod theme;
mod tikz;
mod update;
mod vault;

fn main() {
    app::run(std::env::args_os().nth(1).map(Into::into));
}
