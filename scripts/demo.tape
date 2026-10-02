# Records docs/assets/demo.gif from fixtures/demo-sessions.json.
# Run from the repo root after `cargo build --release`: vhs scripts/demo.tape
Output "docs/assets/demo.gif"
Set Shell "bash"
Set FontSize 16
Set Width 1300
Set Height 560
Set Padding 20
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Hide
Type "export REPO=$PWD HOME=$(cd /tmp && pwd -P)/agentview-demo PS1='$ '; rm -rf $HOME; mkdir -p $HOME/Code/shop; cd $HOME/Code/shop; clear" Enter
Type "$REPO/target/release/agentview --fixture $REPO/fixtures/demo-sessions.json" Enter
Sleep 2s
Show

Sleep 1.5s

Down
Sleep 700ms
Down
Sleep 700ms
Space
Sleep 3s
Space
Sleep 800ms
Down
Sleep 500ms
Down
Sleep 500ms
Down
Sleep 1s

Type "add rate limiting to the checkout API"
Sleep 2.5s

Hide
Ctrl+C
