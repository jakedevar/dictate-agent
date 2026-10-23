import AVFoundation
import Foundation

// Select deterministically across speakers: lexical file order, one utterance
// per speaker per round, 3–15 s inclusive. No random seed or private input.
let args = CommandLine.arguments
let root = URL(fileURLWithPath: args[1])
let output = URL(fileURLWithPath: args[2])
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)!
let files = enumerator.allObjects.compactMap { $0 as? URL }.filter { $0.pathExtension == "flac" }.sorted { $0.path < $1.path }
var bySpeaker: [String: [URL]] = [:]
for file in files {
    let audio = try AVAudioFile(forReading: file)
    let seconds = Double(audio.length) / audio.processingFormat.sampleRate
    if (3...15).contains(seconds) {
        let speaker = file.deletingPathExtension().lastPathComponent.split(separator: "-")[0]
        bySpeaker[String(speaker), default: []].append(file)
    }
}
var selected: [URL] = []
while selected.count < 30 {
    let before = selected.count
    for speaker in bySpeaker.keys.sorted() where selected.count < 30 {
        if !bySpeaker[speaker]!.isEmpty { selected.append(bySpeaker[speaker]!.removeFirst()) }
    }
    precondition(selected.count > before, "not enough eligible samples")
}
for file in selected {
    let id = file.deletingPathExtension().lastPathComponent
    let chapter = file.deletingLastPathComponent()
    let parts = id.split(separator: "-")
    let references = try String(contentsOf: chapter.appendingPathComponent("\(parts[0])-\(parts[1]).trans.txt"), encoding: .utf8)
    let line = references.split(separator: "\n").first { $0.hasPrefix(id + " ") }!
    try String(line.dropFirst(id.count + 1)).write(to: output.appendingPathComponent(id + ".txt"), atomically: true, encoding: .utf8)
    let command = Process()
    command.executableURL = URL(fileURLWithPath: "/usr/bin/afconvert")
    command.arguments = [file.path, output.appendingPathComponent(id + ".wav").path, "-f", "WAVE", "-d", "LEI16@16000", "-c", "1"]
    try command.run()
    command.waitUntilExit()
    precondition(command.terminationStatus == 0)
    print("\(id)\tlibrispeech\t\(id).wav\t\(id).txt")
}
