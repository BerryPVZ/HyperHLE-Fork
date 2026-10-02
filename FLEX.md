# Flex runtime hooks

Open **Quick options → Flex runtime hooks**, choose an installed app with
Previous/Next, turn on Enable Flex, and enter one rule per line. Save before
returning to the picker and launching the app. Switching apps saves the current
profile first; an invalid rule prevents the switch and displays its line number.
Back discards unsaved edits.

To choose a method without typing its name, tap **Browse methods**. The browser
reads the selected app's main executable, lists its classes, and shows instance
(`-`) and class (`+`) methods, their type encodings and inherited guest methods.
Use the filter field and **Apply filter**, or Previous/Next page, to find a class
or selector. Select a method, then tap false, true, nil or skip. Only choices
compatible with its return type are enabled. This inserts a rule into the editor;
press **Save** to apply it. Selecting the same method again replaces its active
rule. You can edit the value to an integer in the editor. **Classes** returns to
the class list; **Done** returns to the editor without inserting a rule.

The browser supports ARM32 Objective-C 2 metadata, including universal binaries
and categories on classes in the main executable. It does not list classes in
separate frameworks or dynamically created classes, or resolve external category
class bindings. Encrypted binaries and unsupported formats display an explanation.

Rules use the app's actual Objective-C class and selector names:

```text
# Illustrative names: replace these with methods from your app.
- NetworkChecker isOnline false
+ LoginGate requiresLogin false
- Session currentToken nil
- Telemetry submit: skip
- Settings retryCount 3
```

`-` selects instance methods; `+` selects class methods. Include every colon in
selectors that take arguments. Matching uses the receiver's exact class name,
including for inherited methods. Prefix a line with `#` to disable it, or delete
it to remove it. Duplicate rules are rejected. Empty profiles have no effect.

Supported returns are `nil` for object/class/pointer results, `true`, `false`,
signed or unsigned 32-bit integers (decimal or `0x` hexadecimal), and `skip` for
void methods. The return encoding is checked at dispatch: incompatible rules,
structs, floating-point results, 64-bit results, and host framework methods are
not overridden. Super dispatch, direct IMP calls, C functions and native machine
code branches are outside this first implementation. This is an Objective-C
method hook editor, not a full Flex 2 binary inspector.

Profiles are scoped by bundle identifier and stored in `touchHLE_flex` under the
emulator's user data directory. Filenames hex-encode the identifier. IPA contents
are unchanged. With Flex enabled, profile edits on disk are reloaded within
500 ms while an app runs; disabling/removing a rule restores normal dispatch.
Malformed or unreadable profiles disable the overrides and emit a log message.
Use `--flex` or `--no-flex` to control hooks when launching from the command line.
Hooks are disabled by default. Returning false for an online check only changes
that method's local result; it does not emulate a server or satisfy its protocol.

The picker also applies the classic glossy icon overlay, zooms the selected icon
over a black background on launch, and animates page changes from either the
arrows or left/right swipes. Swipes do not change pages while Quick Options is open.
