// The always-on-top window marsad opens with P on macOS, run with `osascript -l JavaScript pin.js <status file>`.
// It shows the status file (first line as the title) and closes when the file stops being updated.
ObjC.import('Cocoa');
function run(argv) {
  const path = argv[0];
  const app = $.NSApplication.sharedApplication;
  app.setActivationPolicy(1);
  app.finishLaunching;
  const W = 380, H = 120;
  const vis = $.NSScreen.mainScreen.visibleFrame;
  const rect = $.NSMakeRect(vis.origin.x + vis.size.width - W - 16, vis.origin.y + vis.size.height - H - 16, W, H);
  // Titled, closable, resizable, utility and non-activating, so clicking it leaves focus where it was.
  const style = 1 | 2 | 8 | 16 | 128;
  const win = $.NSPanel.alloc.initWithContentRectStyleMaskBackingDefer(rect, style, 2, false);
  win.title = 'marsad';
  win.level = 3;
  win.collectionBehavior = 1 | 256;
  win.hidesOnDeactivate = false;
  win.releasedWhenClosed = false;
  const label = $.NSTextField.alloc.initWithFrame($.NSMakeRect(10, 6, W - 20, H - 12));
  label.editable = false; label.selectable = false; label.bordered = false; label.drawsBackground = false;
  label.font = $.NSFont.monospacedSystemFontOfSizeWeight(11, 0);
  label.autoresizingMask = 2 | 16;
  label.lineBreakMode = 4;
  win.contentView.addSubview(label);
  win.orderFrontRegardless;
  const fm = $.NSFileManager.defaultManager;
  let last = '';
  while (true) {
    const attrs = fm.attributesOfItemAtPathError(path, null);
    if (attrs.isNil()) break;
    const age = -attrs.fileModificationDate.timeIntervalSinceNow;
    if (age > 15) break;
    const text = ObjC.unwrap($.NSString.stringWithContentsOfFileEncodingError(path, 4, null)) || '';
    if (text !== last) {
      last = text;
      const lines = text.replace(/\n+$/, '').split('\n');
      win.title = lines[0] || 'marsad';
      label.stringValue = lines.slice(1).join('\n');
      const f = win.frame;
      const h = Math.max(60, (lines.length - 1) * 15 + 40);
      win.setFrameDisplay($.NSMakeRect(f.origin.x, f.origin.y + f.size.height - h, f.size.width, h), true);
    }
    const until = $.NSDate.dateWithTimeIntervalSinceNow(1.0);
    while (true) {
      const ev = app.nextEventMatchingMaskUntilDateInModeDequeue(-1, until, $.NSDefaultRunLoopMode, true);
      if (ev.isNil()) break;
      app.sendEvent(ev);
    }
    if (!win.isVisible) break;
  }
}
