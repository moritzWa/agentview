# Work-only browsing

Hide personal sessions without stopping their agents. First follow the
[repo setup in the README](../README.md#work-only-browsing).

Run `agentview view toggle` from another terminal to try it. From a personal native
chat, Agent View switches to the last work chat you opened, or the first available
work chat. If none is available, it shows an empty work-only overview. From the
overview, it stays on the overview. Toggling back restores your previous chat or
draft. Arrow navigation stays unchanged.

`agentview view work` always enables work-only browsing; `agentview view all`
always shows every session. The mode persists across dashboard restarts. No repos
count as work until you configure them. Use absolute paths, not `~` or `$HOME`
inside the JSON. If you set `XDG_CONFIG_HOME`, put the config under that directory
instead of `~/.config`.

## Global shortcut on macOS

You can use [Karabiner-Elements](https://karabiner-elements.pqrs.org/) to run the
command globally. No Agent View companion app or Hammerspoon is needed.

1. Run `command -v agentview` to find the installed command's absolute path.
2. Save the JSON below as
   `~/.config/karabiner/assets/complex_modifications/agentview-work-view.json`.
   Create the parent directory if needed. Replace the example executable path
   with the one from step 1. This adds a rule; don't replace your main
   `karabiner.json`.
3. In Karabiner-Elements, open **Complex Modifications**, choose **Add predefined
   rule**, and enable **Agent View: toggle work-only browsing**.

```json
{
  "title": "Agent View work-only browsing",
  "rules": [
    {
      "description": "Agent View: toggle work-only browsing",
      "manipulators": [
        {
          "type": "basic",
          "from": {"key_code": "f8"},
          "to": [
            {
              "shell_command": "\"/Users/alex/.local/bin/agentview\" view toggle",
              "repeat": false
            }
          ]
        }
      ]
    }
  ]
}
```

This example uses **F8** (Fn+F8 on keyboards whose top row controls media).
Choose another binding if you already use it. Press it while a browser or another
app has focus: Agent View switches in the background without typing into that app.
Press it again to switch back. It doesn't hide the other app or its windows.

If your dashboard uses a custom `XDG_STATE_HOME`, set the same value in the
shortcut's shell command so both use the same saved mode.
