import { Column, Text, Button } from "@w3cos/std"

export default
<Column style={{ gap: 16, padding: 48, alignItems: "center", justifyContent: "center", background: "#102a43" }}>
  <Text style={{ fontSize: 36, color: "#f0f4f8", fontWeight: 700 }}>Shared runtime</Text>
  <Text style={{ fontSize: 18, color: "#9fb3c8" }}>Linked against libw3cos_runtime.so</Text>
  <Button style={{ background: "#2bb0ed", color: "#041422", borderRadius: 8, fontSize: 16 }}>Native AOT</Button>
</Column>
