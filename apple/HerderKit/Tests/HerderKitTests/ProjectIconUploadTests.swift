import CoreGraphics
import Foundation
import Herder
import ImageIO
import Testing
import UniformTypeIdentifiers
@testable import HerderKit

struct ProjectIconUploadTests {
    /// A `width`×`height` image of noise, which compresses badly, encoded as `type`.
    private func picture(width: Int, height: Int, type: UTType = .png) -> Data {
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                                space: CGColorSpaceCreateDeviceRGB(),
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        var generator = SystemRandomNumberGenerator()
        for y in stride(from: 0, to: height, by: 4) {
            for x in stride(from: 0, to: width, by: 4) {
                context.setFillColor(red: .random(in: 0...1, using: &generator), green: .random(in: 0...1, using: &generator),
                                     blue: .random(in: 0...1, using: &generator), alpha: 1)
                context.fill(CGRect(x: x, y: y, width: 4, height: 4))
            }
        }
        let out = NSMutableData()
        let destination = CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        #expect(CGImageDestinationFinalize(destination))
        return out as Data
    }

    private func size(_ png: Data) -> (type: String?, width: Int, height: Int) {
        let source = CGImageSourceCreateWithData(png as CFData, nil)!
        let image = CGImageSourceCreateImageAtIndex(source, 0, nil)!
        return (CGImageSourceGetType(source) as String?, image.width, image.height)
    }

    @Test func aLargePictureBecomesAPNGThatFitsTheBoxAndTheLimit() throws {
        let png = try ProjectIconUpload.png(picture(width: 1200, height: 600, type: .jpeg))
        let (type, width, height) = size(png)
        #expect(type == UTType.png.identifier)
        #expect(width == ProjectIconUpload.side && height == ProjectIconUpload.side / 2)
        #expect(png.count <= Int(maxProjectIconBytes()))
    }

    @Test func aSmallPictureIsNotScaledUp() throws {
        let (_, width, height) = size(try ProjectIconUpload.png(picture(width: 40, height: 40)))
        #expect(width == 40 && height == 40)
    }

    @Test func itIsHalvedUntilItFitsTheLimit() throws {
        let png = try ProjectIconUpload.png(picture(width: 512, height: 512), limit: 20_000)
        #expect(png.count <= 20_000)
        #expect(size(png).width < ProjectIconUpload.side)
    }

    @Test func somethingThatIsNoPictureIsRefused() {
        #expect(throws: ProjectIconUpload.Refused.self) {
            try ProjectIconUpload.png(Data("not a picture".utf8))
        }
    }
}
