# fittotrack-sync
A server that supports FitToTrack's HTTP Post backup target. I couldn't find one, so I made it

## Setup
For maximum convenience install `qrencode` on the server you're using (not required)
```
cargo build --release
target/release/fittotrack-sync setup
```

View more detailed setup instructions in the Wiki
