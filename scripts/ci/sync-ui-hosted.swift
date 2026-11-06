import AppKit
import CoreGraphics
import ApplicationServices
import Vision
let args = CommandLine.arguments
let command = args[1]
if command == "ocr" {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    try VNImageRequestHandler(url: URL(fileURLWithPath: args[2])).perform([request])
    print((request.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n"))
    exit(0)
}
let pid = pid_t(args[2])!
func windows() -> [[String:Any]] {
    let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String:Any]] ?? []
    return rows.filter { ($0[kCGWindowOwnerPID as String] as? Int) == Int(pid) && ($0[kCGWindowLayer as String] as? Int) == 0 }
}
if command == "list" {
    let data = try JSONSerialization.data(withJSONObject: windows(), options: [.sortedKeys])
    print(String(data:data,encoding:.utf8)!)
} else if command == "activate" {
    guard let app = NSRunningApplication(processIdentifier: pid) else { exit(2) }
    app.activate(options: [.activateIgnoringOtherApps])
} else if command == "settings" {
    NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
    Thread.sleep(forTimeInterval: 1)
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource:nil, virtualKey:43, keyDown:down)!
        event.flags = .maskCommand
        event.post(tap:.cghidEventTap)
    }
} else if command == "resize" {
    let app = AXUIElementCreateApplication(pid)
    var raw: CFTypeRef?
    guard AXUIElementCopyAttributeValue(app,kAXWindowsAttribute as CFString,&raw) == .success,
          let rows = raw as? [AXUIElement] else { exit(3) }
    for window in rows {
        var title: CFTypeRef?
        AXUIElementCopyAttributeValue(window,kAXTitleAttribute as CFString,&title)
        if (title as? String)?.contains("Settings") == true {
            var position = CGPoint(x:20,y:45)
            var size = CGSize(width:960,height:650)
            let pv = AXValueCreate(.cgPoint,&position)!
            let sv = AXValueCreate(.cgSize,&size)!
            let p = AXUIElementSetAttributeValue(window,kAXPositionAttribute as CFString,pv)
            let s = AXUIElementSetAttributeValue(window,kAXSizeAttribute as CFString,sv)
            print("position=\(p.rawValue) size=\(s.rawValue)")
        }
    }
} else { exit(4) }
