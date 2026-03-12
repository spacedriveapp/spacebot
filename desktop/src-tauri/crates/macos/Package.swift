// swift-tools-version: 5.5

import PackageDescription

let package = Package(
    name: "sb-desktop-macos",
    platforms: [
        .macOS(.v11),
    ],
    products: [
        .library(
            name: "sb-desktop-macos",
            type: .static,
            targets: ["sb-desktop-macos"]
        ),
    ],
    dependencies: [
        .package(url: "https://github.com/brendonovich/swift-rs", branch: "specta"),
    ],
    targets: [
        .target(
            name: "sb-desktop-macos",
            dependencies: [
                .product(name: "SwiftRs", package: "swift-rs"),
            ],
            path: "src-swift"
        ),
    ]
)
