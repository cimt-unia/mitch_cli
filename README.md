# Mitch Cli

This is the repo of the mitch_cli, used to controll Mitch sensors using a daemon managing the BTLE connections. 

## Requirements 

- linux
- Rust (with the package manager cargo) `https://rust-lang.org/tools/install/`

## Installation

```
cargo install --path .
```

## Usage 

To get an overview of all commands:

```
mitch_cli --help
```

Before using the cli the Daemon needs to be started with:
Tipp: running the command like this attaches the current terminal to the daemon process allowing you to see the logs in case of error.

```
mitch_cli daemon-start
```

if you want to run the daemon in the background and create a log file use this:

```
mitch_cli daemon-start > daemon.lob &
```

To scan for mitches:

```
mitch_cli scan
```

To check status of connected mitches:

```
mitch_cli status
```

To connect to a specific mitch (you get the name from scan i.e.):

```
mitch_cli connect <Name>
```

To disconnect from a specific mitch:

```
mitch_cli disconnect <Name>
```

To set a mitch to streaming mode opening a lsl-stream:

```
mitch_cli record <Name>
```



