import harmonyRuntime from 'libw3cos_harmony_host.so';

@Entry
@Component
struct Index {
  private frameTimer: number = -1

  aboutToAppear(): void {
    this.frameTimer = setInterval(() => {
      harmonyRuntime.frame()
    }, 16)
  }

  aboutToDisappear(): void {
    if (this.frameTimer >= 0) {
      clearInterval(this.frameTimer)
      this.frameTimer = -1
    }
  }

  build() {
    Stack() {
      XComponent({
        id: 'w3cos-content',
        type: XComponentType.SURFACE,
        libraryName: 'w3cos_harmony_host'
      })
        .width('100%')
        .height('100%')
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#0B1220')
  }
}
