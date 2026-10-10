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

struct ProjectIconPictureTests {
    /// Green squares of each of `sides`, as one file of `type`.
    private func image(sides: [Int], type: UTType) -> Data {
        let out = NSMutableData()
        let destination = CGImageDestinationCreateWithData(out, type.identifier as CFString, sides.count, nil)!
        for side in sides {
            let context = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
                                    space: CGColorSpaceCreateDeviceRGB(),
                                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            context.setFillColor(red: 0, green: 1, blue: 0, alpha: 1)
            context.fill(CGRect(x: 0, y: 0, width: side, height: side))
            CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        }
        #expect(CGImageDestinationFinalize(destination))
        return out as Data
    }

    /// The red, green and blue of the pixel `y` rows from the top of `picture`.
    private func pixel(_ picture: CGImage, x: Int, y: Int) -> [UInt8] {
        var rgba = [UInt8](repeating: 0, count: 4)
        let context = CGContext(data: &rgba, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.draw(picture, in: CGRect(x: -x, y: y - picture.height + 1, width: picture.width, height: picture.height))
        return Array(rgba.prefix(3))
    }

    /// Machines find favicon.svg first; iOS showed none, as UIImage reads no SVG.
    @Test func anSvgIconIsDrawnUprightAtTheIconSide() throws {
        let svg = Data("""
            <svg xmlns="http://www.w3.org/2000/svg" width="16" height="8">
              <rect width="16" height="4" fill="#ff0000"/><rect y="4" width="16" height="4" fill="#0000ff"/>
            </svg>
            """.utf8)
        let picture = try #require(ProjectIconImage.picture(svg))
        #expect(picture.width == 256 && picture.height == 128)
        #expect(pixel(picture, x: 128, y: 10) == [255, 0, 0])
        #expect(pixel(picture, x: 128, y: 118) == [0, 0, 255])
    }

    @Test func aRasterIconIsReadAndAnIcoGivesItsLargestImage() throws {
        let png = try #require(ProjectIconImage.picture(image(sides: [40], type: .png)))
        #expect(png.width == 40 && png.height == 40)
        let ico = try #require(ProjectIconImage.picture(image(sides: [16, 48, 32], type: .ico)))
        #expect(ico.width == 48 && ico.height == 48)
    }

    @Test func aFileThatIsNoPictureHasNone() {
        #expect(ProjectIconImage.picture(Data("not a picture".utf8)) == nil)
    }
}
