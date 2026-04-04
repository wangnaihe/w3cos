import { Column, Row, Text, Button, TextInput } from "@w3cos/std"

const agentStatus = signal(0)
const taskCount = signal(3)

export default
<Column style={{ background: "#0a0a12", gap: 0 }}>
  {/* Header */}
  <Row style={{
    padding: 16,
    background: "#12121f",
    justifyContent: "spaceBetween",
    alignItems: "center"
  }}>
    <Row style={{ gap: 10, alignItems: "center" }}>
      <Text style={{ fontSize: 22, color: "#6c5ce7" }}>🤖</Text>
      <Text style={{ fontSize: 18, color: "#e0e0f0", fontWeight: 700 }}>AI Agent Hub</Text>
    </Row>
    <Row style={{ gap: 12, alignItems: "center" }}>
      <Text style={{ fontSize: 13, color: "#00b894" }}>● Connected</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>DOM Access: Layer 1</Text>
    </Row>
  </Row>

  <Row style={{ flexGrow: 1, gap: 0 }}>
    {/* Sidebar: Agent list */}
    <Column style={{ width: "240", padding: 16, gap: 8, background: "#10101c" }}>
      <Text style={{ fontSize: 11, color: "#505070", fontWeight: 700 }}>ACTIVE AGENTS</Text>

      <Column style={{ padding: 12, background: "#1c1c34", borderRadius: 8, gap: 6 }}>
        <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
          <Text style={{ fontSize: 13, color: "#d0d0e0", fontWeight: 600 }}>Code Agent</Text>
          <Text style={{ fontSize: 11, color: "#00b894" }}>Running</Text>
        </Row>
        <Text style={{ fontSize: 11, color: "#606080" }}>Writing filesystem module...</Text>
        <Row style={{ height: "4", borderRadius: 2, background: "#1e1e38" }}>
          <Column style={{ width: "65%", height: "4", borderRadius: 2, background: "#6c5ce7" }} />
        </Row>
      </Column>

      <Column style={{ padding: 12, background: "#1c1c34", borderRadius: 8, gap: 6 }}>
        <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
          <Text style={{ fontSize: 13, color: "#d0d0e0", fontWeight: 600 }}>Review Agent</Text>
          <Text style={{ fontSize: 11, color: "#fdcb6e" }}>Waiting</Text>
        </Row>
        <Text style={{ fontSize: 11, color: "#606080" }}>Queued: PR #42 review</Text>
      </Column>

      <Column style={{ padding: 12, background: "#1c1c34", borderRadius: 8, gap: 6 }}>
        <Row style={{ justifyContent: "spaceBetween", alignItems: "center" }}>
          <Text style={{ fontSize: 13, color: "#d0d0e0", fontWeight: 600 }}>Test Agent</Text>
          <Text style={{ fontSize: 11, color: "#00b894" }}>Running</Text>
        </Row>
        <Text style={{ fontSize: 11, color: "#606080" }}>47/52 tests passed</Text>
        <Row style={{ height: "4", borderRadius: 2, background: "#1e1e38" }}>
          <Column style={{ width: "90%", height: "4", borderRadius: 2, background: "#00b894" }} />
        </Row>
      </Column>

      <Text style={{ fontSize: 11, color: "#505070", fontWeight: 700 }}>CAPABILITIES</Text>
      <Column style={{ gap: 4 }}>
        <Row style={{ gap: 6, alignItems: "center" }}>
          <Text style={{ fontSize: 11, color: "#00b894" }}>✓</Text>
          <Text style={{ fontSize: 11, color: "#a0a0c0" }}>DOM Read (Layer 1)</Text>
        </Row>
        <Row style={{ gap: 6, alignItems: "center" }}>
          <Text style={{ fontSize: 11, color: "#00b894" }}>✓</Text>
          <Text style={{ fontSize: 11, color: "#a0a0c0" }}>DOM Write (Layer 2)</Text>
        </Row>
        <Row style={{ gap: 6, alignItems: "center" }}>
          <Text style={{ fontSize: 11, color: "#00b894" }}>✓</Text>
          <Text style={{ fontSize: 11, color: "#a0a0c0" }}>A11y Tree</Text>
        </Row>
        <Row style={{ gap: 6, alignItems: "center" }}>
          <Text style={{ fontSize: 11, color: "#00b894" }}>✓</Text>
          <Text style={{ fontSize: 11, color: "#a0a0c0" }}>Screenshot + Annotations</Text>
        </Row>
        <Row style={{ gap: 6, alignItems: "center" }}>
          <Text style={{ fontSize: 11, color: "#fdcb6e" }}>○</Text>
          <Text style={{ fontSize: 11, color: "#808090" }}>File System (restricted)</Text>
        </Row>
        <Row style={{ gap: 6, alignItems: "center" }}>
          <Text style={{ fontSize: 11, color: "#e94560" }}>✕</Text>
          <Text style={{ fontSize: 11, color: "#606080" }}>Network (denied)</Text>
        </Row>
      </Column>
    </Column>

    {/* Main: Agent conversation */}
    <Column style={{ flexGrow: 1, padding: 20, gap: 16 }}>
      <Text style={{ fontSize: 18, color: "#e0e0f0", fontWeight: 700 }}>Agent Conversation</Text>

      <Column style={{ flexGrow: 1, gap: 12, overflow: "scroll" }}>
        {/* System message */}
        <Row style={{ gap: 12 }}>
          <Column style={{
            width: "32", height: "32",
            background: "#1c1c34",
            borderRadius: 16,
            alignItems: "center",
            justifyContent: "center"
          }}>
            <Text style={{ fontSize: 14 }}>🖥</Text>
          </Column>
          <Column style={{
            flexGrow: 1,
            padding: 12,
            background: "#141428",
            borderRadius: 8,
            gap: 4
          }}>
            <Text style={{ fontSize: 11, color: "#606080" }}>System</Text>
            <Text style={{ fontSize: 13, color: "#a0a0c0" }}>Agent connected. DOM access granted (Layer 1: read, Layer 2: write). Permission model: interactive.</Text>
          </Column>
        </Row>

        {/* Agent message */}
        <Row style={{ gap: 12 }}>
          <Column style={{
            width: "32", height: "32",
            background: "#6c5ce7",
            borderRadius: 16,
            alignItems: "center",
            justifyContent: "center"
          }}>
            <Text style={{ fontSize: 14 }}>🤖</Text>
          </Column>
          <Column style={{
            flexGrow: 1,
            padding: 12,
            background: "#1c1c34",
            borderRadius: 8,
            gap: 4
          }}>
            <Text style={{ fontSize: 11, color: "#6c5ce7" }}>Code Agent</Text>
            <Text style={{ fontSize: 13, color: "#d0d0e0" }}>I can see the DOM tree of the current application. There are 47 elements, 12 interactive buttons, and 3 text inputs. The accessibility tree shows all elements are properly labeled.</Text>
          </Column>
        </Row>

        {/* User message */}
        <Row style={{ gap: 12, justifyContent: "flexEnd" }}>
          <Column style={{
            padding: 12,
            background: "#6c5ce7",
            borderRadius: 8,
            gap: 4,
            maxWidth: "70%"
          }}>
            <Text style={{ fontSize: 13, color: "#ffffff" }}>Build a file manager component with tree view for the sidebar and list view for the main content.</Text>
          </Column>
          <Column style={{
            width: "32", height: "32",
            background: "#2a2a3e",
            borderRadius: 16,
            alignItems: "center",
            justifyContent: "center"
          }}>
            <Text style={{ fontSize: 14 }}>👤</Text>
          </Column>
        </Row>

        {/* Agent response */}
        <Row style={{ gap: 12 }}>
          <Column style={{
            width: "32", height: "32",
            background: "#6c5ce7",
            borderRadius: 16,
            alignItems: "center",
            justifyContent: "center"
          }}>
            <Text style={{ fontSize: 14 }}>🤖</Text>
          </Column>
          <Column style={{
            flexGrow: 1,
            padding: 12,
            background: "#1c1c34",
            borderRadius: 8,
            gap: 8
          }}>
            <Text style={{ fontSize: 11, color: "#6c5ce7" }}>Code Agent</Text>
            <Text style={{ fontSize: 13, color: "#d0d0e0" }}>I'll create the file manager now. Using the DOM API to build the component tree:</Text>
            <Column style={{ padding: 8, background: "#0f0f1a", borderRadius: 4 }}>
              <Text style={{ fontSize: 12, color: "#74b9ff" }}>document.createElement("div")  // sidebar</Text>
              <Text style={{ fontSize: 12, color: "#74b9ff" }}>document.createElement("div")  // file list</Text>
              <Text style={{ fontSize: 12, color: "#74b9ff" }}>element.style.display = "flex"</Text>
              <Text style={{ fontSize: 12, color: "#74b9ff" }}>element.appendChild(sidebar)</Text>
              <Text style={{ fontSize: 12, color: "#00b894" }}>✓ 24 DOM operations completed in 0.3ms</Text>
            </Column>
          </Column>
        </Row>
      </Column>

      {/* Input */}
      <Row style={{ gap: 8, alignItems: "center" }}>
        <TextInput value="" placeholder="Ask the AI agent..." style={{
          flexGrow: 1,
          fontSize: 14,
          color: "#d0d0e0",
          background: "#1c1c34",
          borderRadius: 8
        }} />
        <Button style={{
          background: "#6c5ce7",
          borderRadius: 8,
          fontSize: 14,
          color: "#ffffff"
        }}>Send</Button>
      </Row>
    </Column>
  </Row>
</Column>
