# Vimcal
## Descripton
This is a calendar app that uses VIM shortucts, it super fast to use, has a short learning curve if you are familiar with VIM and it was written in rust so your computer will like it too.
## USAGE
I tried to make everything as VIM-like as possible, so if you are familiar with it you shouldn't have any issues. If you get confused with using Vimcal press "?" for help.
## Cool features
- Vimcal has .ics support, you can import and export calendars (Exporting can be done with ``` vimcal export all``` or ```vimcal export <calendar name>```)
- There is multiple calendar support and you can even give custom colors to them in HEX
- You can use VISUAL mode to select the duration of your desired event
- Use "/" to search between events
- There is a monthly view as well
- and so much more, go play with it!
## Installation

This project is not yet available in the AUR, for now, you can build and install it from source. I think I will test this app for a week or two before I'll release it pubically. The instructions below assume you have Git, Rust, and Cargo installed.

### Steps to install on linux:

Replace `<project URL>` with the repository’s Git URL:

```bash
git clone <project URL>
cd vimcal
cargo build --release
sudo cp target/release/vimcal /usr/bin/vimcal
```

## DISCLAMER
This was 100% made with AI, I literally haven't even touched a single line of code in this project, be wary. 
