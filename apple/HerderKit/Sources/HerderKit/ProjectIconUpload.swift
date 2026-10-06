import Herder
import ImageIO
import SwiftUI
import UniformTypeIdentifiers
#if os(iOS)
import PhotosUI
#endif

/// A picture an owner picked as a project's icon, made into what the machine keeps: a PNG of
/// at most `side`×`side` pixels and `maxProjectIconBytes()` bytes.
enum ProjectIconUpload {
    struct Refused: LocalizedError {
        let errorDescription: String?
    }

    /// The longest side of an uploaded icon, in pixels.
    static let side = 256

    /// The image in `data`, of any type ImageIO reads, as a PNG scaled down to fit `side`
    /// (never up) and halved again until it fits in `limit` bytes.
    static func png(_ data: Data, limit: Int = Int(maxProjectIconBytes())) throws -> Data {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else {
            throw Refused(errorDescription: "That file is not a picture herder can read.")
        }
        var side = Self.side
        while side >= 16 {
            let options: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: side,
            ]
            guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { break }
            let out = NSMutableData()
            guard let destination = CGImageDestinationCreateWithData(out, UTType.png.identifier as CFString, 1, nil)
            else { break }
            CGImageDestinationAddImage(destination, image, nil)
            guard CGImageDestinationFinalize(destination) else { break }
            if out.length <= limit { return out as Data }
            side /= 2
        }
        throw Refused(errorDescription: "That picture cannot be made into an icon.")
    }
}

/// A project's icon: the one every device shows, a picture to upload in its place on every
/// machine this device owns, and, when one was uploaded, a way back to the icon the machines
/// find in their clones.
struct ProjectIconRow: View {
    let fleet: Fleet
    let project: Project
    /// The background the form holds, shown before the machine lists it.
    let background: String?
    @Binding var error: String?
    @State private var choosingFile = false
    @State private var busy = false
    #if os(iOS)
    @State private var photo: PhotosPickerItem?
    #endif

    var body: some View {
        SettingRow(label: "Icon", detail: detail) {
            HStack(spacing: 10) {
                if busy { ProgressView().controlSize(.small) }
                ProjectIcon(projectId: project.projectId, name: project.name,
                            image: ProjectIconImage(data: fleet.projectIcon(project.projectId)?.data,
                                                    background: background),
                            size: 32)
                if project.iconUploaded {
                    button("Reset") { await upload(nil) }
                }
                #if os(iOS)
                Menu {
                    PhotosPicker(selection: $photo, matching: .images) { Label("Photo Library", systemImage: "photo") }
                    Button { choosingFile = true } label: { Label("Files…", systemImage: "folder") }
                } label: {
                    label("Choose image…")
                }
                #else
                button("Choose image…") { choosingFile = true }
                #endif
            }
            .disabled(busy)
        }
        .fileImporter(isPresented: $choosingFile, allowedContentTypes: [.image]) { result in
            Task { await pick(result) }
        }
        #if os(iOS)
        .onChange(of: photo) {
            guard let photo else { return }
            self.photo = nil
            Task {
                do {
                    guard let data = try await photo.loadTransferable(type: Data.self) else { return }
                    await upload(data)
                } catch {
                    self.error = describe(error)
                }
            }
        }
        #endif
    }

    private var detail: String {
        if project.iconUploaded { return "Uploaded" }
        return project.icon == nil ? "None found in the clone" : "Found in the clone"
    }

    private func label(_ title: String) -> some View {
        Text(title)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Theme.text)
            .padding(.horizontal, 12)
            .frame(height: 32)
            .background(Theme.raised, in: .rect(cornerRadius: 7))
    }

    private func button(_ title: String, action: @escaping () async -> Void) -> some View {
        Button { Task { await action() } } label: { label(title) }
            .buttonStyle(.plain)
    }

    private func pick(_ result: Result<URL, any Error>) async {
        do {
            let url = try result.get()
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            await upload(try Data(contentsOf: url))
        } catch {
            self.error = describe(error)
        }
    }

    /// Sends `data`, made into an icon, to the machines; `nil` clears the uploads.
    private func upload(_ data: Data?) async {
        busy = true
        defer { busy = false }
        do {
            let png = try data.map { try ProjectIconUpload.png($0) }
            try await fleet.setProjectIcon(project.projectId, png: png)
            error = nil
        } catch {
            self.error = describe(error)
        }
    }
}
