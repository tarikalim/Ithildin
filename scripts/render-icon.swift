// Renders assets/icon.svg with a transparent background (qlmanage paints it white).
//   swift scripts/render-icon.swift assets/icon.svg icon1024.png
import AppKit
let (src, dst) = (CommandLine.arguments[1], CommandLine.arguments[2])
let size = 1024
guard let img = NSImage(contentsOfFile: src) else { fatalError("cannot load svg") }
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSColor.clear.set(); NSRect(x: 0, y: 0, width: size, height: size).fill()
img.draw(in: NSRect(x: 0, y: 0, width: size, height: size))
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: dst))
