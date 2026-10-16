// List on-screen windows with their CGWindowID so a specific application
// window can be captured without changing focus.
//
// Usage: swift scripts/list-windows.swift [owner-substring]

import CoreGraphics
import Foundation

let needle = CommandLine.arguments.count > 1 ? CommandLine.arguments[1].lowercased() : nil

guard
    let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
    ) as? [[String: Any]]
else {
    FileHandle.standardError.write(Data("无法读取窗口列表\n".utf8))
    exit(2)
}

for window in windows {
    let owner = (window[kCGWindowOwnerName as String] as? String) ?? ""
    let title = (window[kCGWindowName as String] as? String) ?? ""
    let number = (window[kCGWindowNumber as String] as? Int) ?? 0
    let layer = (window[kCGWindowLayer as String] as? Int) ?? 0
    let bounds = window[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let width = (bounds["Width"] as? Double) ?? 0
    let height = (bounds["Height"] as? Double) ?? 0

    // Only real, reasonably sized windows.
    guard layer == 0, width > 400, height > 300 else { continue }
    if let needle, !(owner.lowercased().contains(needle) || title.lowercased().contains(needle)) {
        continue
    }
    print("\(number)\t\(Int(width))x\(Int(height))\t\(owner)\t\(title)")
}
