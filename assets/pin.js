// The always-on-top window marsad opens with P on macOS, run with `osascript -l JavaScript pin.js <status file>`.
// It shows the status file (first line as the title) and closes when the file stops being updated.
ObjC.import('Cocoa');
function run(argv) {
  const path = argv[0];
  const app = $.NSApplication.sharedApplication;
  app.setActivationPolicy(1);
  app.finishLaunching;
  const W = 420, H = 120;
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
  const font = $.NSFont.monospacedSystemFontOfSizeWeight(11, 0);
  const bold = $.NSFont.monospacedSystemFontOfSizeWeight(11, 0.4);
  label.font = font;
  // Each session row ends with its status, colored as on the dashboard.
  const colors = {
    APPROVE: [$.NSColor.systemYellowColor, bold],
    QUESTION: [$.NSColor.systemPurpleColor, bold],
    ERROR: [$.NSColor.systemRedColor, bold],
    ACTIVE: [$.NSColor.systemGreenColor, font],
    IDLE: [$.NSColor.systemBlueColor, font],
  };
  const levels = { ok: $.NSColor.systemGreenColor, warn: $.NSColor.systemYellowColor, crit: $.NSColor.systemRedColor };
  const styled = (marked) => {
    // The first line may be the gauge row: `[ok]ctx 18%[/] · 795.5k left · [warn]5h 64%[/] ↻2:10pm`.
    // Its marked parts show bold in their level's color, the rest dimmed.
    const gauges = [];
    let body = marked, dimmed = 0;
    if (/^\[(ok|warn|crit)\]/.test(marked)) {
      const nl = marked.indexOf('\n');
      const first = nl < 0 ? marked : marked.slice(0, nl);
      let plain = '';
      for (const part of first.split(/(\[(?:ok|warn|crit)\].*?\[\/\])/)) {
        const m = part.match(/^\[(ok|warn|crit)\](.*)\[\/\]$/);
        if (m) { gauges.push([plain.length, m[2].length, levels[m[1]]]); plain += m[2]; } else plain += part;
      }
      body = plain + (nl < 0 ? '' : marked.slice(nl));
      dimmed = plain.length;
    }
    const out = $.NSMutableAttributedString.new;
    out.mutableString.setString(body);
    const all = $.NSMakeRange(0, body.length);
    out.addAttributeValueRange($.NSFontAttributeName, font, all);
    out.addAttributeValueRange($.NSForegroundColorAttributeName, $.NSColor.labelColor, all);
    out.addAttributeValueRange($.NSForegroundColorAttributeName, $.NSColor.secondaryLabelColor, $.NSMakeRange(0, dimmed));
    for (const [at, len, color] of gauges) {
      out.addAttributeValueRange($.NSForegroundColorAttributeName, color, $.NSMakeRange(at, len));
      out.addAttributeValueRange($.NSFontAttributeName, bold, $.NSMakeRange(at, len));
    }
    let at = 0;
    for (const line of body.split('\n')) {
      const m = line.match(/([A-Z]+) \S+$/);
      const c = m && colors[m[1]];
      if (c) {
        const range = $.NSMakeRange(at + m.index, line.length - m.index);
        out.addAttributeValueRange($.NSForegroundColorAttributeName, c[0], range);
        out.addAttributeValueRange($.NSFontAttributeName, c[1], range);
      }
      at += line.length + 1;
    }
    return out;
  };
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
      label.attributedStringValue = styled(lines.slice(1).join('\n'));
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
