import { Column, Row, Text } from "@w3cos/std"

export default
<Column style={{ gap: 0, padding: 32, background: "#0f0f1a", width: 400, borderRadius: 16 }}>
  <Text style={{ fontSize: 28, color: "#ffffff", fontWeight: 700, marginBottom: 16 }}>Settings</Text>

  <Text style={{ fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 }}>Appearance</Text>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Theme</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>Dark</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Font Size</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>16px</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Language</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>English</Text>
  </Row>

  <Text style={{ fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 }}>Notifications</Text>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Push Notifications</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>Enabled</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Email Digest</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>Weekly</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Sound</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>On</Text>
  </Row>

  <Text style={{ fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 }}>Account</Text>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Username</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>w3cos_user</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Email</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>user@w3cos.dev</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Plan</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>Free</Text>
  </Row>

  <Text style={{ fontSize: 13, color: "#e94560", fontWeight: 600, marginTop: 16, marginBottom: 8 }}>About</Text>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>Version</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>1.0.0</Text>
  </Row>
  <Row style={{ justifyContent: "space-between", padding: 16, borderBottom: "1px solid #1a1a2e", width: "100%" }}>
    <Text style={{ fontSize: 16, color: "#e0e0e0" }}>License</Text>
    <Text style={{ fontSize: 16, color: "#888899" }}>MIT</Text>
  </Row>
</Column>
