# Shared fixes from the Buddyman 2, Break the Cookie, and Charlie logs

This change starts at trunk commit e4b5b2da and preserves the existing Flex
browser, launcher shine/zoom/swipe features, and earlier rendering changes.
No app identifier or game-specific branch is introduced.

## Changes

- Property-list serialization checks signed NSDate intervals before converting
  to SystemTime. Invalid/unrepresentable dates return nil and an error
  description instead of panicking. Valid negative dates remain negative.
  NSDate's Unix-epoch accessor uses arithmetic instead of a fallible host date.
- EAGL readback selects the presented draw framebuffer as its read source and
  restores separate read/draw bindings, including temporary-FBO failure paths.
  Native ES1 drivers are not queried with unsupported split-binding enums.
- NSInvocation copies full argument widths, preserves positions of unset
  arguments using zero-filled slots, and sign-extends narrow signed scalars.
  Object/block annotations remain one signature token. NSObject uses real
  guest method encodings when available instead of treating every argument
  as an object.
- NSValue preserves generic struct/array/union bytes using the existing guest
  type-layout parser, including three-byte color values from the logs.
- NSSet supports adding one object and keyed decoding of NS.objects. Set
  copies and unions use members rather than nested sets or indexed access.
- NSUnionRange is exported with the existing struct-return ABI.
- NSProxy has root-class allocation and lifetime operations. This does not
  implement general Objective-C invocation forwarding.
- ADBannerView embeds UIView host state and calls UIView initializers/dealloc,
  eliminating the wrong-host-type borrowing path shown in two logs.

## Validation and limits

The date and invocation tests and the mocked framebuffer tests live beside
production code. An isolated Rust harness runs those exact helpers and the
real GLES trait without linking the emulator's native CPU/audio libraries:
seven new tests and one existing GLES test pass. The original framebuffer
implementation fails two of the three new framebuffer tests.

A Rust-only `cargo check --lib --tests` uses temporary local bypasses for the
Dynarmic and OpenAL native build scripts; these bypasses are not committed.
This is not a full native build or a gameplay test. The initial full check was
blocked by missing Boost and Linux audio development dependencies.

The logs alone do not establish the source of the guest "Invalid pixel
format" exceptions or Buddyman's invalid free after quitting. These are not
claimed fixed. NSProxy forwarding and non-void NSInvocation return handling
remain incomplete.

## Runtime retest

1. Build this branch with the normal platform dependencies.
2. Run Charlie through the save operation that previously panicked; verify
   valid saved dates reload and invalid dates fail without terminating the
   emulator.
3. Run Break the Cookie beyond its splash and exercise scene changes and
   callbacks. Check whether the repeated EAGL 0x506 errors stop.
4. Run Buddyman, inspect colors and mission/upgrade state, then save/relaunch.
5. Check Flex's class/method browser and the launcher animations remain
   available. Capture fresh logs if pixel-format exceptions, rendering faults,
   or exit-time frees remain.
