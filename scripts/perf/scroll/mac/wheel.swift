// wheel <pid> <events> <deltaPx> <interval_ms> : posts pixel wheel events to the HID tap, like
// `okqa scroll`, but without capturing the screen (a capture loads WindowServer and perturbs frame
// timing). Positive delta scrolls down. Prints how long the stream really took.
import AppKit
import CoreGraphics
import Foundation

func now() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1e6 }
let a = CommandLine.arguments
guard a.count >= 5, let pid = pid_t(a[1]), let n = Int(a[2]), let delta = Int32(a[3]), let iv = Double(a[4]) else {
    FileHandle.standardError.write("usage: wheel <pid> <events> <deltaPx> <interval_ms>\n".data(using: .utf8)!)
    exit(2)
}
func mainWindow(_ pid: pid_t) -> CGRect? {
    guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] else { return nil }
    var best: CGRect? = nil
    for w in list {
        guard (w[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
              (w[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
              let b = w[kCGWindowBounds as String] as? [String: NSNumber] else { continue }
        let r = CGRect(x: b["X"]!.doubleValue, y: b["Y"]!.doubleValue, width: b["Width"]!.doubleValue, height: b["Height"]!.doubleValue)
        if r.width > 200 && r.height > 200 && (best == nil || r.width * r.height > best!.width * best!.height) { best = r }
    }
    return best
}
guard let win = mainWindow(pid) else { print("no window"); exit(1) }
NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
usleep(600_000)
guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else {
    print("refusing: pid \(pid) is not frontmost"); exit(1)
}
let pt = CGPoint(x: win.midX - 100, y: win.midY)
CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: pt, mouseButton: .left)?.post(tap: .cghidEventTap)
usleep(400_000)
let t0 = now()
var sent = 0
while sent < n {
    if now() - t0 >= Double(sent) * iv {
        let e = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: -delta, wheel2: 0, wheel3: 0)!
        e.location = pt
        e.post(tap: .cghidEventTap)
        sent += 1
    } else {
        usleep(300)
    }
}
print(String(format: "sent=%d stream_ms=%.0f", sent, now() - t0))
