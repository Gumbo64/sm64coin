package io.github.gumbo64.sm64coin

import android.os.Bundle
import android.view.View
import android.graphics.Color
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

private val NAVY = Color.parseColor("#0f1220") // the site's background

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // The page is always dark: navy bars with light icons whatever the phone's theme (on Android 10+ with gesture navigation the bars are transparent over the window background, which is the same navy).
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(NAVY),
      navigationBarStyle = SystemBarStyle.dark(NAVY)
    )
    super.onCreate(savedInstanceState)
    // targetSdk 37 draws edge to edge: keep the page (the WebView) clear of the status bar, the navigation bar, a display
    // cutout and the keyboard by padding the activity's content view with their insets (the page itself needs no safe-area CSS).
    val root = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
      val b = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime()
      )
      v.setPadding(b.left, b.top, b.right, b.bottom)
      WindowInsetsCompat.CONSUMED
    }
    ViewCompat.requestApplyInsets(root)
  }
}
