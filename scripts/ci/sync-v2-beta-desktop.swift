import AppKit
import CoreGraphics
import ApplicationServices
import Vision
let args = CommandLine.arguments
let command = args[1]
if command == "launch" {
    let config = NSWorkspace.OpenConfiguration()
    config.activates = true
    config.createsNewApplicationInstance = true
    config.arguments = ["--vault", args[3], "--note", "seed.md"]
    config.environment = ["OKILUM_STATE_DIR": args[4]]
    NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: args[2]), configuration: config) { app, error in
        if let app = app { print(app.processIdentifier); fflush(stdout); exit(0) }
        fputs(String(describing: error), stderr); exit(5)
    }
    RunLoop.main.run()
    exit(6)
}
if command == "click" {
    let point = CGPoint(x:Double(args[2])!,y:Double(args[3])!)
    for type in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
        CGEvent(mouseEventSource:nil, mouseType:type, mouseCursorPosition:point, mouseButton:.left)!.post(tap:.cghidEventTap)
        Thread.sleep(forTimeInterval:0.1)
    }
    exit(0)
}
if command == "paste" || command == "go-folder" || command == "return" {
    let key: CGKeyCode = command == "paste" ? 9 : command == "go-folder" ? 5 : 36
    let flags: CGEventFlags = command == "paste" ? .maskCommand : command == "go-folder" ? [.maskCommand,.maskShift] : []
    for down in [true,false] {
        let event = CGEvent(keyboardEventSource:nil,virtualKey:key,keyDown:down)!
        event.flags = flags
        event.post(tap:.cghidEventTap)
    }
    exit(0)
}
if command == "escape" {
    for down in [true,false] { CGEvent(keyboardEventSource:nil,virtualKey:53,keyDown:down)!.post(tap:.cghidEventTap) }
    exit(0)
}
if command == "ocr" || command == "ocrjson" {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    try VNImageRequestHandler(url: URL(fileURLWithPath: args[2])).perform([request])
    if command == "ocrjson" {
        let display = CGDisplayBounds(CGMainDisplayID())
        let rows: [[String:Any]] = (request.results ?? []).compactMap { observation in
            guard let text = observation.topCandidates(1).first?.string else { return nil }
            let box = observation.boundingBox
            return ["text":text,"x":box.midX * display.width,"y":(1-box.midY) * display.height]
        }
        print(String(data:try JSONSerialization.data(withJSONObject:rows),encoding:.utf8)!)
    } else { print((request.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")) }
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
