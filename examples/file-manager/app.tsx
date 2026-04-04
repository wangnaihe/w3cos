import { Column, Row, Text, Button, TextInput } from "@w3cos/std"

const selectedFile = signal(0)
const viewMode = signal(0)

export default
<Column style={{ background: "#0f0f1a", gap: 0 }}>
  {/* Title bar */}
  <Row style={{
    height: "40",
    background: "#1a1a2e",
    padding: 12,
    alignItems: "center",
    justifyContent: "spaceBetween"
  }}>
    <Row style={{ gap: 8, alignItems: "center" }}>
      <Text style={{ fontSize: 16 }}>📁</Text>
      <Text style={{ fontSize: 14, color: "#e0e0f0", fontWeight: 600 }}>File Manager</Text>
    </Row>
    <Row style={{ gap: 4 }}>
      <Button style={{ fontSize: 12, color: "#808090", borderRadius: 4, background: "#2a2a3e" }}>—</Button>
      <Button style={{ fontSize: 12, color: "#808090", borderRadius: 4, background: "#2a2a3e" }}>□</Button>
      <Button style={{ fontSize: 12, color: "#e94560", borderRadius: 4, background: "#2a2a3e" }}>✕</Button>
    </Row>
  </Row>

  {/* Toolbar */}
  <Row style={{
    padding: 8,
    gap: 8,
    background: "#141428",
    alignItems: "center"
  }}>
    <Button style={{ fontSize: 12, color: "#a0a0c0", background: "#1c1c34", borderRadius: 6 }}>← Back</Button>
    <Button style={{ fontSize: 12, color: "#a0a0c0", background: "#1c1c34", borderRadius: 6 }}>→ Forward</Button>
    <Button style={{ fontSize: 12, color: "#a0a0c0", background: "#1c1c34", borderRadius: 6 }}>↑ Up</Button>
    <Row style={{
      flexGrow: 1,
      background: "#1c1c34",
      borderRadius: 6,
      padding: 6,
      alignItems: "center"
    }}>
      <Text style={{ fontSize: 12, color: "#606080" }}>/home/user/Documents</Text>
    </Row>
    <Button style={{ fontSize: 12, color: "#a0a0c0", background: "#1c1c34", borderRadius: 6 }} onClick="toggle:viewMode">☰ View</Button>
  </Row>

  {/* Main content */}
  <Row style={{ flexGrow: 1, gap: 0 }}>
    {/* Sidebar — directory tree */}
    <Column style={{
      width: "200",
      background: "#10101c",
      padding: 12,
      gap: 2,
      overflow: "scroll"
    }}>
      <Text style={{ fontSize: 11, color: "#505070", fontWeight: 700 }}>PLACES</Text>
      <Row style={{ padding: 8, borderRadius: 6, background: "#6c5ce7", gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>🏠</Text>
        <Text style={{ fontSize: 13, color: "#ffffff" }}>Home</Text>
      </Row>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>📄</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>Documents</Text>
      </Row>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>📷</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>Pictures</Text>
      </Row>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>🎵</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>Music</Text>
      </Row>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>🎬</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>Videos</Text>
      </Row>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>⬇</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>Downloads</Text>
      </Row>

      <Text style={{ fontSize: 11, color: "#505070", fontWeight: 700 }}>DEVICES</Text>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>💾</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>System (512 GB)</Text>
      </Row>
      <Row style={{ padding: 8, borderRadius: 6, gap: 8, alignItems: "center" }}>
        <Text style={{ fontSize: 13 }}>📀</Text>
        <Text style={{ fontSize: 13, color: "#a0a0c0" }}>USB Drive</Text>
      </Row>
    </Column>

    {/* File list */}
    <Column style={{ flexGrow: 1, padding: 12, gap: 4 }}>
      {/* Column headers */}
      <Row style={{
        padding: 8,
        gap: 16,
        borderRadius: 4,
        background: "#141428"
      }}>
        <Text style={{ width: "40%", fontSize: 11, color: "#606080", fontWeight: 700 }}>Name</Text>
        <Text style={{ width: "15%", fontSize: 11, color: "#606080", fontWeight: 700 }}>Size</Text>
        <Text style={{ width: "20%", fontSize: 11, color: "#606080", fontWeight: 700 }}>Modified</Text>
        <Text style={{ width: "15%", fontSize: 11, color: "#606080", fontWeight: 700 }}>Type</Text>
      </Row>

      {/* Files */}
      <Row style={{ padding: 8, gap: 16, borderRadius: 4, alignItems: "center" }} onClick="set:selectedFile:1">
        <Row style={{ width: "40%", gap: 8, alignItems: "center" }}>
          <Text style={{ fontSize: 14 }}>📁</Text>
          <Text style={{ fontSize: 13, color: "#d0d0e0" }}>projects</Text>
        </Row>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>—</Text>
        <Text style={{ width: "20%", fontSize: 12, color: "#808090" }}>Mar 22, 2026</Text>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>Folder</Text>
      </Row>

      <Row style={{ padding: 8, gap: 16, borderRadius: 4, alignItems: "center" }} onClick="set:selectedFile:2">
        <Row style={{ width: "40%", gap: 8, alignItems: "center" }}>
          <Text style={{ fontSize: 14 }}>📁</Text>
          <Text style={{ fontSize: 13, color: "#d0d0e0" }}>w3cos</Text>
        </Row>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>—</Text>
        <Text style={{ width: "20%", fontSize: 12, color: "#808090" }}>Mar 22, 2026</Text>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>Folder</Text>
      </Row>

      <Row style={{ padding: 8, gap: 16, borderRadius: 4, alignItems: "center" }} onClick="set:selectedFile:3">
        <Row style={{ width: "40%", gap: 8, alignItems: "center" }}>
          <Text style={{ fontSize: 14 }}>📄</Text>
          <Text style={{ fontSize: 13, color: "#d0d0e0" }}>README.md</Text>
        </Row>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>4.2 KB</Text>
        <Text style={{ width: "20%", fontSize: 12, color: "#808090" }}>Mar 20, 2026</Text>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>Markdown</Text>
      </Row>

      <Row style={{ padding: 8, gap: 16, borderRadius: 4, alignItems: "center" }} onClick="set:selectedFile:4">
        <Row style={{ width: "40%", gap: 8, alignItems: "center" }}>
          <Text style={{ fontSize: 14 }}>🦀</Text>
          <Text style={{ fontSize: 13, color: "#d0d0e0" }}>Cargo.toml</Text>
        </Row>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>1.1 KB</Text>
        <Text style={{ width: "20%", fontSize: 12, color: "#808090" }}>Mar 22, 2026</Text>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>TOML</Text>
      </Row>

      <Row style={{ padding: 8, gap: 16, borderRadius: 4, alignItems: "center" }} onClick="set:selectedFile:5">
        <Row style={{ width: "40%", gap: 8, alignItems: "center" }}>
          <Text style={{ fontSize: 14 }}>📜</Text>
          <Text style={{ fontSize: 13, color: "#d0d0e0" }}>app.tsx</Text>
        </Row>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>2.8 KB</Text>
        <Text style={{ width: "20%", fontSize: 12, color: "#808090" }}>Mar 22, 2026</Text>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>TypeScript</Text>
      </Row>

      <Row style={{ padding: 8, gap: 16, borderRadius: 4, alignItems: "center" }} onClick="set:selectedFile:6">
        <Row style={{ width: "40%", gap: 8, alignItems: "center" }}>
          <Text style={{ fontSize: 14 }}>🖼</Text>
          <Text style={{ fontSize: 13, color: "#d0d0e0" }}>screenshot.png</Text>
        </Row>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>245 KB</Text>
        <Text style={{ width: "20%", fontSize: 12, color: "#808090" }}>Mar 18, 2026</Text>
        <Text style={{ width: "15%", fontSize: 12, color: "#808090" }}>PNG Image</Text>
      </Row>

      {/* Status bar */}
      <Row style={{
        padding: 8,
        gap: 16,
        alignItems: "center",
        justifyContent: "spaceBetween"
      }}>
        <Text style={{ fontSize: 11, color: "#606080" }}>6 items</Text>
        <Text style={{ fontSize: 11, color: "#606080" }}>253 KB used</Text>
      </Row>
    </Column>
  </Row>
</Column>
