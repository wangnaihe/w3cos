import { Column, Row, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 24, padding: 40, alignItems: "center", justifyContent: "center", background: "#0f0f1a", width: 360 }}>
  <Column style={{ gap: 4, marginBottom: 16 }}>
    <Text style={{ fontSize: 32, color: "#ffffff", fontWeight: 700 }}>Login</Text>
    <Text style={{ fontSize: 14, color: "#888899" }}>Sign in to your account</Text>
  </Column>
  <Column style={{ gap: 16, width: "100%" }}>
    <Text style={{ fontSize: 14, color: "#a0a0b0", marginBottom: -8 }}>Email</Text>
    <Text style={{ padding: 14, background: "#1a1a2e", borderRadius: 10, borderWidth: 1, borderColor: "#2a2a3e", color: "#606070", fontSize: 16, width: "100%" }}>you@example.com</Text>
    <Text style={{ fontSize: 14, color: "#a0a0b0", marginTop: 8, marginBottom: -8 }}>Password</Text>
    <Text style={{ padding: 14, background: "#1a1a2e", borderRadius: 10, borderWidth: 1, borderColor: "#2a2a3e", color: "#606070", fontSize: 16, width: "100%" }}>••••••••</Text>
  </Column>
  <Row style={{ justifyContent: "space-between", width: "100%", marginTop: 4 }}>
    <Text style={{ fontSize: 13, color: "#e94560" }}>Remember me</Text>
    <Text style={{ fontSize: 13, color: "#e94560" }}>Forgot password?</Text>
  </Row>
  <Button style={{ width: "100%", padding: 16, background: "#e94560", color: "#ffffff", borderRadius: 10, fontSize: 18, fontWeight: 600, marginTop: 8 }}>Sign In</Button>
  <Row style={{ gap: 16, marginTop: 16 }}>
    <Text style={{ fontSize: 14, color: "#888899" }}>Don't have an account?</Text>
    <Text style={{ fontSize: 14, color: "#e94560", fontWeight: 600 }}>Sign Up</Text>
  </Row>
</Column>
