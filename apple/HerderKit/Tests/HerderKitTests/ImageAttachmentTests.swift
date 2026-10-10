import CoreGraphics
import Foundation
import Herder
import ImageIO
@testable import HerderKit
import Testing
import UniformTypeIdentifiers

struct ImageAttachmentTests {
    @Test func aSmallPNGGoesAsItIsAndOtherImagesBecomeJPEG() throws {
        // A 1×1 PNG.
        let png = try #require(Data(base64Encoded: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg=="))
        let image = try ImageAttachment.make(png)
        #expect(image.mediaType == "image/png")
        #expect(image.data == png)
        let converted = try ImageAttachment.make(try picture(width: 40, height: 30, as: .tiff))
        #expect(converted.mediaType == "image/jpeg")
        #expect(throws: (any Error).self) { try ImageAttachment.make(Data("not a picture".utf8)) }
    }

    /// A phone camera's photo is well under the daemon's byte limit, yet goes scaled down to
    /// what a provider looks at, as the TUI sends it.
    @Test func aCameraPhotoGoesScaledToTheLongEdge() throws {
        let photo = try picture(width: 4032, height: 3024, as: .jpeg)
        #expect(photo.count <= Int(maxImageBytes()))
        let image = try ImageAttachment.make(photo)
        #expect(image.mediaType == "image/jpeg")
        #expect(try size(image.data) == (ImageAttachment.longEdge, 1536))
    }

    /// A photo over the daemon's byte limit still goes.
    @Test func aPhotoOverTheByteLimitStillGoes() throws {
        let photo = try picture(width: 4032, height: 3024, as: .png, noise: true)
        #expect(photo.count > Int(maxImageBytes()))
        let image = try ImageAttachment.make(photo)
        #expect(image.data.count <= Int(maxImageBytes()))
        #expect(try size(image.data).width <= ImageAttachment.longEdge)
    }

    /// A tall screenshot stays a PNG, so its text stays sharp.
    @Test func aLargeScreenshotStaysPNG() throws {
        let screenshot = try picture(width: 1179, height: 2556, as: .png)
        let image = try ImageAttachment.make(screenshot)
        #expect(image.mediaType == "image/png")
        #expect(try size(image.data).height == ImageAttachment.longEdge)
    }

    /// An image's type is the one its bytes are in.
    @Test func aJPEGGoesAsJPEGWhateverItIsCalled() throws {
        let jpeg = try picture(width: 40, height: 30, as: .jpeg)
        let image = try ImageAttachment.make(jpeg)
        #expect(image.mediaType == "image/jpeg")
        #expect(image.data == jpeg)
        #expect(ImageAttachment.images(in: [["public.png": jpeg]]).first?.mediaType == "image/jpeg")
    }

    /// A picture of this size in this type: a gradient, or noise, which compresses badly.
    private func picture(width: Int, height: Int, as type: UTType, noise: Bool = false) throws -> Data {
        let context = try #require(CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
        let pixels = try #require(context.data).bindMemory(to: UInt8.self, capacity: context.bytesPerRow * height)
        var seed: UInt32 = 1
        for y in 0..<height {
            for x in 0..<width {
                let at = y * context.bytesPerRow + x * 4
                for channel in 0..<3 {
                    seed = seed &* 1_664_525 &+ 1_013_904_223
                    pixels[at + channel] = noise ? UInt8(truncatingIfNeeded: seed >> 24)
                        : UInt8(truncatingIfNeeded: (x + y * channel) * 255 / (width + height))
                }
            }
        }
        let image = try #require(context.makeImage())
        let out = NSMutableData()
        let destination = try #require(CGImageDestinationCreateWithData(out, type.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, nil)
        #expect(CGImageDestinationFinalize(destination))
        return out as Data
    }

    private func size(_ data: Data) throws -> (width: Int, height: Int) {
        let source = try #require(CGImageSourceCreateWithData(data as CFData, nil))
        let properties = try #require(CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any])
        return (try #require(properties[kCGImagePropertyPixelWidth] as? Int),
                try #require(properties[kCGImagePropertyPixelHeight] as? Int))
    }
}
