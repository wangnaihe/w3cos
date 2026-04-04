import { Column, Row, Text, Button, TextInput } from "@w3cos/std"

const tabIndex = signal(0)

export default
<Column style={{ background: "#0c0c14", gap: 0 }}>
  {/* Title bar */}
  <Row style={{
    height: "36",
    background: "#1a1a2e",
    padding: 8,
    alignItems: "center",
    justifyContent: "spaceBetween"
  }}>
    <Row style={{ gap: 8, alignItems: "center" }}>
      <Text style={{ fontSize: 14 }}>⌨</Text>
      <Text style={{ fontSize: 13, color: "#e0e0f0", fontWeight: 600 }}>W3C OS Terminal</Text>
    </Row>
    <Row style={{ gap: 4 }}>
      <Button style={{ fontSize: 11, color: "#808090", borderRadius: 4, background: "#2a2a3e" }}>—</Button>
      <Button style={{ fontSize: 11, color: "#808090", borderRadius: 4, background: "#2a2a3e" }}>□</Button>
      <Button style={{ fontSize: 11, color: "#e94560", borderRadius: 4, background: "#2a2a3e" }}>✕</Button>
    </Row>
  </Row>

  {/* Tab bar */}
  <Row style={{ background: "#141428", padding: 4, gap: 2 }}>
    <Row style={{
      padding: 6,
      borderRadius: 6,
      background: "#1c1c34",
      gap: 6,
      alignItems: "center"
    }} onClick="set:tabIndex:0">
      <Text style={{ fontSize: 12, color: "#d0d0e0" }}>bash</Text>
      <Text style={{ fontSize: 10, color: "#505070" }}>✕</Text>
    </Row>
    <Row style={{
      padding: 6,
      borderRadius: 6,
      gap: 6,
      alignItems: "center"
    }} onClick="set:tabIndex:1">
      <Text style={{ fontSize: 12, color: "#808090" }}>python3</Text>
      <Text style={{ fontSize: 10, color: "#505070" }}>✕</Text>
    </Row>
    <Button style={{
      fontSize: 12,
      color: "#505070",
      background: "#0c0c14",
      borderRadius: 6
    }}>+ New</Button>
  </Row>

  {/* Terminal content */}
  <Column style={{
    flexGrow: 1,
    padding: 12,
    gap: 2,
    background: "#0c0c14",
    overflow: "scroll"
  }}>
    <Text style={{ fontSize: 13, color: "#00b894" }}>W3C OS Terminal v0.1.0</Text>
    <Text style={{ fontSize: 13, color: "#606080" }}>Type 'help' for available commands.</Text>
    <Text style={{ fontSize: 13, color: "#606080" }}></Text>

    <Row style={{ gap: 0 }}>
      <Text style={{ fontSize: 13, color: "#6c5ce7" }}>user@w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>:</Text>
      <Text style={{ fontSize: 13, color: "#74b9ff" }}>~/projects/w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>$ </Text>
      <Text style={{ fontSize: 13, color: "#d0d0e0" }}>cargo build --release</Text>
    </Row>

    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>   Compiling w3cos-std v0.1.0</Text>
    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>   Compiling w3cos-dom v0.1.0</Text>
    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>   Compiling w3cos-runtime v0.1.0</Text>
    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>   Compiling w3cos-compiler v0.1.0</Text>
    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>   Compiling w3cos-cli v0.1.0</Text>
    <Text style={{ fontSize: 13, color: "#00b894" }}>    Finished `release` profile [optimized] target(s) in 42.3s</Text>
    <Text style={{ fontSize: 13, color: "#606080" }}></Text>

    <Row style={{ gap: 0 }}>
      <Text style={{ fontSize: 13, color: "#6c5ce7" }}>user@w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>:</Text>
      <Text style={{ fontSize: 13, color: "#74b9ff" }}>~/projects/w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>$ </Text>
      <Text style={{ fontSize: 13, color: "#d0d0e0" }}>ls -la target/release/w3cos</Text>
    </Row>

    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>-rwxr-xr-x  1 user user 2.4M Mar 22 14:32 target/release/w3cos</Text>
    <Text style={{ fontSize: 13, color: "#606080" }}></Text>

    <Row style={{ gap: 0 }}>
      <Text style={{ fontSize: 13, color: "#6c5ce7" }}>user@w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>:</Text>
      <Text style={{ fontSize: 13, color: "#74b9ff" }}>~/projects/w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>$ </Text>
      <Text style={{ fontSize: 13, color: "#d0d0e0" }}>w3cos build examples/showcase/app.tsx -o showcase --release</Text>
    </Row>

    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>⚡ Transpiling examples/showcase/app.tsx → Rust...</Text>
    <Text style={{ fontSize: 13, color: "#a0a0c0" }}>🔨 Compiling native binary...</Text>
    <Text style={{ fontSize: 13, color: "#00b894" }}>✅ Output: ./showcase (2.4 MB)</Text>
    <Text style={{ fontSize: 13, color: "#606080" }}></Text>

    {/* Input line */}
    <Row style={{ gap: 0, alignItems: "center" }}>
      <Text style={{ fontSize: 13, color: "#6c5ce7" }}>user@w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>:</Text>
      <Text style={{ fontSize: 13, color: "#74b9ff" }}>~/projects/w3cos</Text>
      <Text style={{ fontSize: 13, color: "#808090" }}>$ </Text>
      <TextInput value="" placeholder="" style={{ fontSize: 13, color: "#d0d0e0", background: "#0c0c14", flexGrow: 1 }} />
    </Row>
  </Column>

  {/* Status bar */}
  <Row style={{
    height: "24",
    background: "#141428",
    padding: 6,
    gap: 16,
    alignItems: "center"
  }}>
    <Text style={{ fontSize: 11, color: "#606080" }}>bash</Text>
    <Text style={{ fontSize: 11, color: "#606080" }}>80×24</Text>
    <Text style={{ fontSize: 11, color: "#606080" }}>UTF-8</Text>
    <Text style={{ fontSize: 11, color: "#00b894" }}>●</Text>
  </Row>
</Column>
