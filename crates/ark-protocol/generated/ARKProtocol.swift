// Generated from protocol.schema.json. Do not edit.

import Foundation

public enum JSONValue: Codable, Equatable, Sendable {
case null
    case bool(Bool)
    case number(Double)
    case string(String)
case array([JSONValue])
    case object([String: JSONValue])

public init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
if value.decodeNil() { self = .null }
        else if let decoded = try? value.decode(Bool.self) { self = .bool(decoded) }
else if let decoded = try? value.decode(Double.self) { self = .number(decoded) }
else if let decoded = try? value.decode(String.self) { self = .string(decoded) }
else if let decoded = try? value.decode([JSONValue].self) { self = .array(decoded) }
else { self = .object(try value.decode([String: JSONValue].self)) }
    }

public func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
switch self {
        case .null: try value.encodeNil()
        case let .bool(v): try value.encode(v)
case let .number(v): try value.encode(v)
        case let .string(v): try value.encode(v)
case let .array(v): try value.encode(v)
        case let .object(v): try value.encode(v)
        }
    }
}

public struct DecimalString: Codable, Hashable, Sendable, Comparable, ExpressibleByStringLiteral {
public let rawValue: String
    public init(_ value: String) throws {
guard !value.isEmpty, value.allSatisfy({ $0.isASCII && $0.isNumber }) else { throw DecimalStringError.invalid(value) }
let normalized = value.drop(while: { $0 == "0" })
        rawValue = normalized.isEmpty ? "0" : String(normalized)
    }
public init(stringLiteral value: String) { self = try! DecimalString(value) }
public init(from decoder: Decoder) throws { try self.init(decoder.singleValueContainer().decode(String.self)) }
public func encode(to encoder: Encoder) throws { var value = encoder.singleValueContainer(); try value.encode(rawValue) }
public static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue.count == rhs.rawValue.count ? lhs.rawValue < rhs.rawValue : lhs.rawValue.count < rhs.rawValue.count }
}
public enum DecimalStringError: Error, Equatable { case invalid(String) }

public struct AccountPairRequest: Codable, Equatable, Sendable {
    public let deviceName: String
    public let installationId: String
    public let routeId: String

    public init(
        deviceName: String,
        installationId: String,
        routeId: String
    ) {
        self.deviceName = deviceName
        self.installationId = installationId
        self.routeId = routeId
    }
}

public struct ApprovalChoice: Codable, Equatable, Sendable {
    public let destructive: Bool
    public let id: String
    public let label: String
    public let systemImage: String?

    public init(
        destructive: Bool,
        id: String,
        label: String,
        systemImage: String? = nil
    ) {
        self.destructive = destructive
        self.id = id
        self.label = label
        self.systemImage = systemImage
    }
}

public struct AttentionItem: Codable, Equatable, Sendable {
    public let choices: [ApprovalChoice]
    public let computer: String
    public let detail: String
    public let id: String
    public let kind: String
    public let project: String
    public let teammate: String
    public let title: String

    public init(
        choices: [ApprovalChoice],
        computer: String,
        detail: String,
        id: String,
        kind: String,
        project: String,
        teammate: String,
        title: String
    ) {
        self.choices = choices
        self.computer = computer
        self.detail = detail
        self.id = id
        self.kind = kind
        self.project = project
        self.teammate = teammate
        self.title = title
    }
}

public struct Bootstrap: Codable, Equatable, Sendable {
    public let protocols: [ProtocolVersion]
    public let runtimeVersion: String
    public let updateUrl: String?

    public init(
        protocols: [ProtocolVersion],
        runtimeVersion: String,
        updateUrl: String? = nil
    ) {
        self.protocols = protocols
        self.runtimeVersion = runtimeVersion
        self.updateUrl = updateUrl
    }
}

public struct ChatConnectionContinueInput: Codable, Equatable, Sendable {
    public let expectedRevision: String
    public let requestId: String

    public init(
        expectedRevision: String,
        requestId: String
    ) {
        self.expectedRevision = expectedRevision
        self.requestId = requestId
    }
}

public struct ChatConnectionRequest: Codable, Equatable, Sendable {
    public let display: String
    public let id: String
    public let offerId: String
    public let revision: String
    public let scopeRef: String?
    public let state: String

    public init(
        display: String,
        id: String,
        offerId: String,
        revision: String,
        scopeRef: String? = nil,
        state: String
    ) {
        self.display = display
        self.id = id
        self.offerId = offerId
        self.revision = revision
        self.scopeRef = scopeRef
        self.state = state
    }
}

public struct ChatMessage: Codable, Equatable, Sendable {
    public let author: String
    public let body: String
    public let createdAt: String
    public let delivery: String?
    public let id: String
    public let role: String

    public init(
        author: String,
        body: String,
        createdAt: String,
        delivery: String? = nil,
        id: String,
        role: String
    ) {
        self.author = author
        self.body = body
        self.createdAt = createdAt
        self.delivery = delivery
        self.id = id
        self.role = role
    }
}

public struct ChatModelInput: Codable, Equatable, Sendable {
    public let model: ModelSelection

    public init(
        model: ModelSelection
    ) {
        self.model = model
    }
}

public struct ChatSummary: Codable, Equatable, Sendable {
    public let computer: String
    public let id: String
    public let participants: [String]?
    public let preview: String
    public let project: String
    public let title: String
    public let unread: Bool
    public let updatedAt: String

    public init(
        computer: String,
        id: String,
        participants: [String]? = nil,
        preview: String,
        project: String,
        title: String,
        unread: Bool,
        updatedAt: String
    ) {
        self.computer = computer
        self.id = id
        self.participants = participants
        self.preview = preview
        self.project = project
        self.title = title
        self.unread = unread
        self.updatedAt = updatedAt
    }
}

public struct ChatsSnapshot: Codable, Equatable, Sendable {
    public let chats: [ChatSummary]
    public let sequence: String

    public init(
        chats: [ChatSummary],
        sequence: String
    ) {
        self.chats = chats
        self.sequence = sequence
    }
}

public struct ComputerSummary: Codable, Equatable, Sendable {
    public let name: String
    public let projectTerminal: Bool
    public let state: String
    public let unavailableReason: String?

    public init(
        name: String,
        projectTerminal: Bool,
        state: String,
        unavailableReason: String? = nil
    ) {
        self.name = name
        self.projectTerminal = projectTerminal
        self.state = state
        self.unavailableReason = unavailableReason
    }
}

public struct ComputersSnapshot: Codable, Equatable, Sendable {
    public let computers: [ComputerSummary]
    public let here: String

    public init(
        computers: [ComputerSummary],
        here: String
    ) {
        self.computers = computers
        self.here = here
    }
}

public struct ConversationModel: Codable, Equatable, Sendable {
    public let canSelect: Bool
    public let display: String
    public let selection: ModelSelection?

    public init(
        canSelect: Bool,
        display: String,
        selection: ModelSelection? = nil
    ) {
        self.canSelect = canSelect
        self.display = display
        self.selection = selection
    }
}

public struct ConversationSnapshot: Codable, Equatable, Sendable {
    public let activeTurnId: String?
    public let approvals: [AttentionItem]
    public let chat: ChatSummary
    public let connectionRequests: [ChatConnectionRequest]?
    public let messages: [ChatMessage]
    public let model: ConversationModel?
    public let results: [ResultSummary]

    public init(
        activeTurnId: String? = nil,
        approvals: [AttentionItem],
        chat: ChatSummary,
        connectionRequests: [ChatConnectionRequest]? = nil,
        messages: [ChatMessage],
        model: ConversationModel? = nil,
        results: [ResultSummary]
    ) {
        self.activeTurnId = activeTurnId
        self.approvals = approvals
        self.chat = chat
        self.connectionRequests = connectionRequests
        self.messages = messages
        self.model = model
        self.results = results
    }
}

public struct ErrorEnvelope: Codable, Equatable, Sendable {
    public let code: String
    public let display: String
    public let retryAfterSeconds: Int?

    public init(
        code: String,
        display: String,
        retryAfterSeconds: Int? = nil
    ) {
        self.code = code
        self.display = display
        self.retryAfterSeconds = retryAfterSeconds
    }
}

public struct EventEnvelope: Codable, Equatable, Sendable {
    public let affects: [String]
    public let id: String
    public let sequence: String
    public let terminal: TerminalEvent?
    public let type: String

    public init(
        affects: [String],
        id: String,
        sequence: String,
        terminal: TerminalEvent? = nil,
        type: String
    ) {
        self.affects = affects
        self.id = id
        self.sequence = sequence
        self.terminal = terminal
        self.type = type
    }
}

public struct Hello: Codable, Equatable, Sendable {
    public let capabilities: [String]
    public let limits: Limits
    public let minimumClientMinor: Int
    public let mutationEpoch: String
    public let nextMutationCounter: String
    public let protocolMajor: Int
    public let protocolMinor: Int
    public let routeId: String?

    public init(
        capabilities: [String],
        limits: Limits,
        minimumClientMinor: Int,
        mutationEpoch: String,
        nextMutationCounter: String,
        protocolMajor: Int,
        protocolMinor: Int,
        routeId: String? = nil
    ) {
        self.capabilities = capabilities
        self.limits = limits
        self.minimumClientMinor = minimumClientMinor
        self.mutationEpoch = mutationEpoch
        self.nextMutationCounter = nextMutationCounter
        self.protocolMajor = protocolMajor
        self.protocolMinor = protocolMinor
        self.routeId = routeId
    }
}

public struct HomeSnapshot: Codable, Equatable, Sendable {
    public let attention: [AttentionItem]
    public let mostRecentChat: ChatSummary?
    public let recent: [ResultSummary]
    public let sequence: String
    public let working: [WorkingItem]

    public init(
        attention: [AttentionItem],
        mostRecentChat: ChatSummary? = nil,
        recent: [ResultSummary],
        sequence: String,
        working: [WorkingItem]
    ) {
        self.attention = attention
        self.mostRecentChat = mostRecentChat
        self.recent = recent
        self.sequence = sequence
        self.working = working
    }
}

public struct Limits: Codable, Equatable, Sendable {
    public let eventBytes: Int
    public let pageSize: Int
    public let terminalReplayBytes: Int
    public let terminalWriteBytes: Int
    public let uploadBytes: Int
    public let uploadPartBytes: Int

    public init(
        eventBytes: Int,
        pageSize: Int,
        terminalReplayBytes: Int,
        terminalWriteBytes: Int,
        uploadBytes: Int,
        uploadPartBytes: Int
    ) {
        self.eventBytes = eventBytes
        self.pageSize = pageSize
        self.terminalReplayBytes = terminalReplayBytes
        self.terminalWriteBytes = terminalWriteBytes
        self.uploadBytes = uploadBytes
        self.uploadPartBytes = uploadPartBytes
    }
}

public struct ModelDefault: Codable, Equatable, Sendable {
    public let display: String
    public let selection: ModelSelection?

    public init(
        display: String,
        selection: ModelSelection? = nil
    ) {
        self.display = display
        self.selection = selection
    }
}

public struct ModelEffort: Codable, Equatable, Sendable {
    public let id: String
    public let title: String

    public init(
        id: String,
        title: String
    ) {
        self.id = id
        self.title = title
    }
}

public struct ModelProvider: Codable, Equatable, Sendable {
    public let connectionId: String?
    public let id: String
    public let models: [ModelSummary]
    public let title: String

    public init(
        connectionId: String? = nil,
        id: String,
        models: [ModelSummary],
        title: String
    ) {
        self.connectionId = connectionId
        self.id = id
        self.models = models
        self.title = title
    }
}

public struct ModelSelection: Codable, Equatable, Sendable {
    public let effortId: String?
    public let spec: String

    public init(
        effortId: String? = nil,
        spec: String
    ) {
        self.effortId = effortId
        self.spec = spec
    }
}

public struct ModelSummary: Codable, Equatable, Sendable {
    public let defaultEffortId: String?
    public let efforts: [ModelEffort]
    public let spec: String
    public let title: String

    public init(
        defaultEffortId: String? = nil,
        efforts: [ModelEffort],
        spec: String,
        title: String
    ) {
        self.defaultEffortId = defaultEffortId
        self.efforts = efforts
        self.spec = spec
        self.title = title
    }
}

public struct ModelsSnapshot: Codable, Equatable, Sendable {
    public let defaultModel: ModelDefault
    public let providers: [ModelProvider]
    public let routeId: String

    public init(
        defaultModel: ModelDefault,
        providers: [ModelProvider],
        routeId: String
    ) {
        self.defaultModel = defaultModel
        self.providers = providers
        self.routeId = routeId
    }
}

public struct MutationReceipt: Codable, Equatable, Sendable {
    public let acceptedAt: String
    public let display: String
    public let id: String
    public let status: String

    public init(
        acceptedAt: String,
        display: String,
        id: String,
        status: String
    ) {
        self.acceptedAt = acceptedAt
        self.display = display
        self.id = id
        self.status = status
    }
}

public struct MutationRequest: Codable, Equatable, Sendable {
    public let counter: String
    public let epoch: String
    public let input: JSONValue
    public let inputDigest: String
    public let operation: String
    public let target: String

    public init(
        counter: String,
        epoch: String,
        input: JSONValue,
        inputDigest: String,
        operation: String,
        target: String
    ) {
        self.counter = counter
        self.epoch = epoch
        self.input = input
        self.inputDigest = inputDigest
        self.operation = operation
        self.target = target
    }
}

public struct NativePushPresenceRequest: Codable, Equatable, Sendable {
    public let inForeground: Bool
    public let registrationId: String

    public init(
        inForeground: Bool,
        registrationId: String
    ) {
        self.inForeground = inForeground
        self.registrationId = registrationId
    }
}

public struct NativePushRegistrationRequest: Codable, Equatable, Sendable {
    public let expiresAt: String
    public let registrationId: String
    public let runtimeRouteId: String
    public let sendCapability: String

    public init(
        expiresAt: String,
        registrationId: String,
        runtimeRouteId: String,
        sendCapability: String
    ) {
        self.expiresAt = expiresAt
        self.registrationId = registrationId
        self.runtimeRouteId = runtimeRouteId
        self.sendCapability = sendCapability
    }
}

public struct NativePushRegistrationResponse: Codable, Equatable, Sendable {
    public let expiresAt: String
    public let registrationId: String
    public let runtimeRouteId: String

    public init(
        expiresAt: String,
        registrationId: String,
        runtimeRouteId: String
    ) {
        self.expiresAt = expiresAt
        self.registrationId = registrationId
        self.runtimeRouteId = runtimeRouteId
    }
}

public struct NativePushRemoveRequest: Codable, Equatable, Sendable {
    public let registrationId: String

    public init(
        registrationId: String
    ) {
        self.registrationId = registrationId
    }
}

public struct NativePushResolution: Codable, Equatable, Sendable {
    public let attentionId: String
    public let chatId: String?
    public let itemId: String?
    public let state: String

    public init(
        attentionId: String,
        chatId: String? = nil,
        itemId: String? = nil,
        state: String
    ) {
        self.attentionId = attentionId
        self.chatId = chatId
        self.itemId = itemId
        self.state = state
    }
}

public struct NewChatRequest: Codable, Equatable, Sendable {
    public let model: ModelSelection?
    public let project: String
    public let requestId: String?

    public init(
        model: ModelSelection? = nil,
        project: String,
        requestId: String? = nil
    ) {
        self.model = model
        self.project = project
        self.requestId = requestId
    }
}

public struct PairRequest: Codable, Equatable, Sendable {
    public let code: String
    public let deviceName: String
    public let installationNonce: String

    public init(
        code: String,
        deviceName: String,
        installationNonce: String
    ) {
        self.code = code
        self.deviceName = deviceName
        self.installationNonce = installationNonce
    }
}

public struct PairResponse: Codable, Equatable, Sendable {
    public let credential: String
    public let deviceId: String
    public let expiresAt: String
    public let mutationEpoch: String
    public let nextMutationCounter: String

    public init(
        credential: String,
        deviceId: String,
        expiresAt: String,
        mutationEpoch: String,
        nextMutationCounter: String
    ) {
        self.credential = credential
        self.deviceId = deviceId
        self.expiresAt = expiresAt
        self.mutationEpoch = mutationEpoch
        self.nextMutationCounter = nextMutationCounter
    }
}

public struct ProjectSummary: Codable, Equatable, Sendable {
    public let computer: String
    public let hasFolder: Bool
    public let id: String
    public let name: String

    public init(
        computer: String,
        hasFolder: Bool,
        id: String,
        name: String
    ) {
        self.computer = computer
        self.hasFolder = hasFolder
        self.id = id
        self.name = name
    }
}

public struct ProjectsSnapshot: Codable, Equatable, Sendable {
    public let projects: [ProjectSummary]
    public let sequence: String

    public init(
        projects: [ProjectSummary],
        sequence: String
    ) {
        self.projects = projects
        self.sequence = sequence
    }
}

public struct ProtocolVersion: Codable, Equatable, Sendable {
    public let latestMinor: Int
    public let major: Int

    public init(
        latestMinor: Int,
        major: Int
    ) {
        self.latestMinor = latestMinor
        self.major = major
    }
}

public struct ProviderAuthorizationAttempt: Codable, Equatable, Sendable {
    public let authorizationUrl: String?
    public let connectionId: String
    public let display: String
    public let expiresAt: String
    public let id: String
    public let state: String
    public let userCode: String?

    public init(
        authorizationUrl: String? = nil,
        connectionId: String,
        display: String,
        expiresAt: String,
        id: String,
        state: String,
        userCode: String? = nil
    ) {
        self.authorizationUrl = authorizationUrl
        self.connectionId = connectionId
        self.display = display
        self.expiresAt = expiresAt
        self.id = id
        self.state = state
        self.userCode = userCode
    }
}

public struct ProviderAuthorizationBeginRequest: Codable, Equatable, Sendable {
    public let requestId: String
    public let routeId: String

    public init(
        requestId: String,
        routeId: String
    ) {
        self.requestId = requestId
        self.routeId = routeId
    }
}

public struct ProviderAuthorizationCodeRequest: Codable, Equatable, Sendable {
    public let code: String

    public init(
        code: String
    ) {
        self.code = code
    }
}

public struct ProviderConnectionSummary: Codable, Equatable, Sendable {
    public let display: String
    public let id: String
    public let state: String
    public let title: String

    public init(
        display: String,
        id: String,
        state: String,
        title: String
    ) {
        self.display = display
        self.id = id
        self.state = state
        self.title = title
    }
}

public struct ProviderConnectionsSnapshot: Codable, Equatable, Sendable {
    public let connections: [ProviderConnectionSummary]
    public let routeId: String

    public init(
        connections: [ProviderConnectionSummary],
        routeId: String
    ) {
        self.connections = connections
        self.routeId = routeId
    }
}

public struct ResultSummary: Codable, Equatable, Sendable {
    public let byteCount: Int?
    public let createdAt: String
    public let downloadPath: String?
    public let id: String
    public let kind: String
    public let previewText: String?
    public let project: String
    public let title: String

    public init(
        byteCount: Int? = nil,
        createdAt: String,
        downloadPath: String? = nil,
        id: String,
        kind: String,
        previewText: String? = nil,
        project: String,
        title: String
    ) {
        self.byteCount = byteCount
        self.createdAt = createdAt
        self.downloadPath = downloadPath
        self.id = id
        self.kind = kind
        self.previewText = previewText
        self.project = project
        self.title = title
    }
}

public struct TerminalEndRequest: Codable, Equatable, Sendable {
    public let generation: String
    public let handle: String

    public init(
        generation: String,
        handle: String
    ) {
        self.generation = generation
        self.handle = handle
    }
}

public struct TerminalEvent: Codable, Equatable, Sendable {
    public let cursor: String?
    public let data: String?
    public let ended: Bool
    public let gap: Bool
    public let generation: String
    public let handle: String

    public init(
        cursor: String? = nil,
        data: String? = nil,
        ended: Bool,
        gap: Bool,
        generation: String,
        handle: String
    ) {
        self.cursor = cursor
        self.data = data
        self.ended = ended
        self.gap = gap
        self.generation = generation
        self.handle = handle
    }
}

public struct TerminalOpenRequest: Codable, Equatable, Sendable {
    public let cols: Int
    public let computer: String?
    public let copyLargeProject: Bool
    public let projectId: String
    public let rows: Int

    public init(
        cols: Int,
        computer: String? = nil,
        copyLargeProject: Bool,
        projectId: String,
        rows: Int
    ) {
        self.cols = cols
        self.computer = computer
        self.copyLargeProject = copyLargeProject
        self.projectId = projectId
        self.rows = rows
    }
}

public struct TerminalOpenResponse: Codable, Equatable, Sendable {
    public let computer: String
    public let cursor: String
    public let cwd: String
    public let ended: Bool
    public let generation: String
    public let handle: String
    public let history: String
    public let project: ProjectSummary
    public let reattached: Bool

    public init(
        computer: String,
        cursor: String,
        cwd: String,
        ended: Bool,
        generation: String,
        handle: String,
        history: String,
        project: ProjectSummary,
        reattached: Bool
    ) {
        self.computer = computer
        self.cursor = cursor
        self.cwd = cwd
        self.ended = ended
        self.generation = generation
        self.handle = handle
        self.history = history
        self.project = project
        self.reattached = reattached
    }
}

public struct TerminalOperationResponse: Codable, Equatable, Sendable {
    public let accepted: Bool

    public init(
        accepted: Bool
    ) {
        self.accepted = accepted
    }
}

public struct TerminalReadRequest: Codable, Equatable, Sendable {
    public let after: String
    public let generation: String
    public let handle: String

    public init(
        after: String,
        generation: String,
        handle: String
    ) {
        self.after = after
        self.generation = generation
        self.handle = handle
    }
}

public struct TerminalReadResponse: Codable, Equatable, Sendable {
    public let base: String
    public let cursor: String
    public let data: String
    public let ended: Bool
    public let gap: Bool
    public let generation: String

    public init(
        base: String,
        cursor: String,
        data: String,
        ended: Bool,
        gap: Bool,
        generation: String
    ) {
        self.base = base
        self.cursor = cursor
        self.data = data
        self.ended = ended
        self.gap = gap
        self.generation = generation
    }
}

public struct TerminalResizeRequest: Codable, Equatable, Sendable {
    public let cols: Int
    public let generation: String
    public let handle: String
    public let rows: Int

    public init(
        cols: Int,
        generation: String,
        handle: String,
        rows: Int
    ) {
        self.cols = cols
        self.generation = generation
        self.handle = handle
        self.rows = rows
    }
}

public struct TerminalWriteRequest: Codable, Equatable, Sendable {
    public let data: String
    public let generation: String
    public let handle: String
    public let sequence: String

    public init(
        data: String,
        generation: String,
        handle: String,
        sequence: String
    ) {
        self.data = data
        self.generation = generation
        self.handle = handle
        self.sequence = sequence
    }
}

public struct WorkingItem: Codable, Equatable, Sendable {
    public let chatId: String
    public let computer: String
    public let elapsedSeconds: Int
    public let id: String
    public let phase: String
    public let step: String
    public let teammate: String
    public let title: String

    public init(
        chatId: String,
        computer: String,
        elapsedSeconds: Int,
        id: String,
        phase: String,
        step: String,
        teammate: String,
        title: String
    ) {
        self.chatId = chatId
        self.computer = computer
        self.elapsedSeconds = elapsedSeconds
        self.id = id
        self.phase = phase
        self.step = step
        self.teammate = teammate
        self.title = title
    }
}
