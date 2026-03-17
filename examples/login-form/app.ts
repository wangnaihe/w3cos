import { Column, Row, Text, Button } from "@w3cos/std"

export default Column({
  style: { gap: 24, padding: 40, alignItems: "center", justifyContent: "center", background: "#0f0f1a", width: 360 },
  children: [
    Column({
      style: { gap: 4, marginBottom: 16 },
      children: [
        Text("Login", { style: { fontSize: 32, color: "#ffffff", fontWeight: 700 } }),
        Text("Sign in to your account", { style: { fontSize: 14, color: "#888899" } }),
      ]
    }),
    Column({
      style: { gap: 16, width: "100%" },
      children: [
        Text("Email", { style: { fontSize: 14, color: "#a0a0b0", marginBottom: -8 } }),
        Text("you@example.com", { style: { padding: 14, background: "#1a1a2e", borderRadius: 10, borderWidth: 1, borderColor: "#2a2a3e", color: "#606070", fontSize: 16, width: "100%" } }),
        Text("Password", { style: { fontSize: 14, color: "#a0a0b0", marginTop: 8, marginBottom: -8 } }),
        Text("••••••••", { style: { padding: 14, background: "#1a1a2e", borderRadius: 10, borderWidth: 1, borderColor: "#2a2a3e", color: "#606070", fontSize: 16, width: "100%" } }),
      ]
    }),
    Row({
      style: { justifyContent: "space-between", width: "100%", marginTop: 4 },
      children: [
        Text("Remember me", { style: { fontSize: 13, color: "#e94560" } }),
        Text("Forgot password?", { style: { fontSize: 13, color: "#e94560" } }),
      ]
    }),
    Button("Sign In", { style: { width: "100%", padding: 16, background: "#e94560", color: "#ffffff", borderRadius: 10, fontSize: 18, fontWeight: 600, marginTop: 8 } }),
    Row({
      style: { gap: 16, marginTop: 16 },
      children: [
        Text("Don't have an account?", { style: { fontSize: 14, color: "#888899" } }),
        Text("Sign Up", { style: { fontSize: 14, color: "#e94560", fontWeight: 600 } }),
      ]
    }),
  ]
})
