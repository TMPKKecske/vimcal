# Vimcal
## Descripton
This is a calendar app that uses VIM shortucts, it super fast to use, has a short learning curve if you are familiar with VIM and it was written in rust so your computer will like it too.
## USAGE
I tried to make everything as VIM-like as possible, so if you are familiar with it you shouldn't have any issues. If you get confused with using Vimcal press "?" for help.
## Cool features
- Vimcal has .ics support, you can import and export calendars
- There is multiple calendar support and you can even give custom colors to them in HEX
- You can use VISUAL mode to select the duration of your desired event
- Use "/" to search between events
- There is a monthly view as well
## Installation
I haven't yet uploaded this to the AUR, I want to use it for a few weeks to make sure it can be uploaded to the public. Before installing you should install rust and cargo to build the project. 
If you wanna use it, simply download the project with:
'''git clone <project URL>'''
Then build it:
'''cargo build --release'''
Find the folder for the build:
'''cd vimcal/target/release/vimcal'''
Copy it to /usr/bin/
'''Sudo cp vimcal/target/release/vimcal /usr/bin/vimcal'''

## DISCLAMER
This was 100% made with AI, I literally haven't even touched a single line of code in this project, be wary. 
