# red - rusty editor

red is a research/passion project to create a modal text editor in Rust from scratch, using as minimal dependencies as possible.

![red screenshot](docs/screenshot.png)

## Current status

This editor is being actively built on a series of streams and videos published to my CoderSauce YouTube channel here:

https://youtube.com/@CoderSauce

It is my intention to keep it stable starting at the first alpha release, but there are no guarantees. As such, use it at your discretion. Bad things can happen to your files, so don't use it yet for anything critical.

If you want to collaborate or discuss red's features, usage or anything, join our Discord:

https://discord.gg/5PWvAUNRHU

## Quickstart

This is a preliminary version of the final readme, but this section should get you up and running.

Clone the git repo

```shell
git clone https://github.com/codersauce/red.git
cd red
```

Install it

```shell
cargo install --path .
```

Configure it

```shell
mkdir -p ~/.config/red
cp default_config.toml ~/.config/red/config.toml
cp -R themes ~/.config/red
```

Run it

```shell
red <file-to-edit>
```

## Testing

If you find any issues be more welcome to report them. Since red's still very immature and unstable make sure you check the known issues first:

https://github.com/codersauce/red/issues/

Thank you so much for trying it! <3
