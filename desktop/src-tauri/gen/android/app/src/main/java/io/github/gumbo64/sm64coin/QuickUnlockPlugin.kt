package io.github.gumbo64.sm64coin

import android.app.Activity
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class QuickWrapArgs {
  var secret: String = ""
}

@InvokeArg
class QuickUnwrapArgs {
  var blob: String = ""
}

/**
 * Quick-unlock for the wallet (see src/quick.rs): seals a secret (the wallet's password) with an Android Keystore AES-256-GCM key
 * that needs the user to authenticate for every use, through a BiometricPrompt with a CryptoObject. The key is hardware-backed
 * (StrongBox when the phone has one), accepts strong biometrics (and on Android 11+ the device PIN / pattern / password), and is
 * invalidated by Android when biometrics are enrolled or removed. Nothing is cached: no secret or key lives in this class.
 * Blob = base64(iv (12 bytes) + ciphertext + GCM tag). Rejections carry the codes cancelled / lockout / invalidated / unavailable /
 * failed, which the page turns into messages.
 */
@TauriPlugin
class QuickUnlockPlugin(private val activity: Activity) : Plugin(activity) {
  private companion object {
    const val ALIAS = "sm64coin.quickunlock.v1"
    const val PROVIDER = "AndroidKeyStore"
    const val IV_LEN = 12
  }

  // Android 11+ lets the device credential unlock a CryptoObject key; before that only strong biometrics do.
  private val authenticators: Int =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) BIOMETRIC_STRONG or DEVICE_CREDENTIAL else BIOMETRIC_STRONG

  private fun keystore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

  private fun deleteKey() {
    try { keystore().deleteEntry(ALIAS) } catch (_: Exception) { }
  }

  private fun newKey(): SecretKey {
    fun gen(strongBox: Boolean): SecretKey {
      val b = KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .setUserAuthenticationRequired(true)
        .setInvalidatedByBiometricEnrollment(true)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        b.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
      } else {
        @Suppress("DEPRECATION")
        b.setUserAuthenticationValidityDurationSeconds(-1) // every use
      }
      if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) b.setIsStrongBoxBacked(true)
      val g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
      g.init(b.build())
      return g.generateKey()
    }
    deleteKey()
    return try { gen(true) } catch (_: Exception) { deleteKey(); gen(false) } // no StrongBox: the TEE-backed Keystore
  }

  private fun prompt(invoke: Invoke, title: String, cipher: Cipher, done: (Cipher) -> ByteArray) {
    val act = activity as FragmentActivity
    act.runOnUiThread {
      try {
        val info = BiometricPrompt.PromptInfo.Builder()
          .setTitle(title)
          .setSubtitle("SM64Coin wallet")
          .setAllowedAuthenticators(authenticators)
          .setConfirmationRequired(false)
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) info.setNegativeButtonText("Cancel")
        val bp = BiometricPrompt(act, ContextCompat.getMainExecutor(act), object : BiometricPrompt.AuthenticationCallback() {
          override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
            val c = result.cryptoObject?.cipher
            if (c == null) { invoke.reject("No cipher from the prompt", "quick_failed"); return }
            try { done(c) } catch (e: Exception) { invoke.reject("Could not unlock: ${e.javaClass.simpleName}", "quick_failed") }
          }
          override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
            val code = when (errorCode) {
              BiometricPrompt.ERROR_USER_CANCELED, BiometricPrompt.ERROR_NEGATIVE_BUTTON, BiometricPrompt.ERROR_CANCELED -> "cancelled"
              BiometricPrompt.ERROR_LOCKOUT, BiometricPrompt.ERROR_LOCKOUT_PERMANENT -> "lockout"
              BiometricPrompt.ERROR_NO_BIOMETRICS, BiometricPrompt.ERROR_HW_NOT_PRESENT, BiometricPrompt.ERROR_HW_UNAVAILABLE, BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL -> "unavailable"
              else -> "quick_failed"
            }
            invoke.reject(errString.toString(), code)
          }
          // a wrong finger is not final: the prompt stays open and retries by itself
        })
        bp.authenticate(info.build(), BiometricPrompt.CryptoObject(cipher))
      } catch (e: Exception) {
        invoke.reject("Could not show the prompt: ${e.javaClass.simpleName}", "quick_failed")
      }
    }
  }

  @Command
  fun status(invoke: Invoke) {
    val r = JSObject()
    val s = BiometricManager.from(activity).canAuthenticate(authenticators)
    r.put("available", s == BiometricManager.BIOMETRIC_SUCCESS)
    if (s != BiometricManager.BIOMETRIC_SUCCESS) {
      r.put("reason", when (s) {
        BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED -> "none_enrolled"
        BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE -> "no_hardware"
        BiometricManager.BIOMETRIC_ERROR_HW_UNAVAILABLE -> "hw_unavailable"
        else -> "unsupported"
      })
    }
    r.put("deviceCredential", Build.VERSION.SDK_INT >= Build.VERSION_CODES.R)
    invoke.resolve(r)
  }

  @Command
  fun wrap(invoke: Invoke) {
    val args = invoke.parseArgs(QuickWrapArgs::class.java)
    if (args.secret.isEmpty()) { invoke.reject("Nothing to seal", "quick_failed"); return }
    try {
      val cipher = Cipher.getInstance("AES/GCM/NoPadding")
      cipher.init(Cipher.ENCRYPT_MODE, newKey())
      val plain = args.secret.toByteArray(Charsets.UTF_8)
      prompt(invoke, "Turn on quick unlock", cipher) { c ->
        val ct = c.doFinal(plain)
        plain.fill(0)
        val out = JSObject()
        out.put("blob", Base64.encodeToString(c.iv + ct, Base64.NO_WRAP))
        invoke.resolve(out)
        ct
      }
    } catch (e: Exception) {
      deleteKey()
      invoke.reject("Could not set up the Keystore key: ${e.javaClass.simpleName}", "unavailable")
    }
  }

  @Command
  fun unwrap(invoke: Invoke) {
    val args = invoke.parseArgs(QuickUnwrapArgs::class.java)
    val blob = try { Base64.decode(args.blob, Base64.NO_WRAP) } catch (_: Exception) { null }
    if (blob == null || blob.size <= IV_LEN) { invoke.reject("Damaged quick-unlock data", "invalidated"); return }
    try {
      val key = keystore().getKey(ALIAS, null) as? SecretKey
      if (key == null) { invoke.reject("The quick-unlock key is gone", "invalidated"); return }
      val cipher = Cipher.getInstance("AES/GCM/NoPadding")
      cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, blob, 0, IV_LEN))
      prompt(invoke, "Unlock the wallet", cipher) { c ->
        val pt = c.doFinal(blob, IV_LEN, blob.size - IV_LEN)
        val out = JSObject()
        out.put("secret", String(pt, Charsets.UTF_8))
        pt.fill(0)
        invoke.resolve(out)
        pt
      }
    } catch (e: KeyPermanentlyInvalidatedException) {
      deleteKey()
      invoke.reject("Your fingerprints or screen lock changed: quick unlock was turned off", "invalidated")
    } catch (e: Exception) {
      invoke.reject("Could not start the unlock: ${e.javaClass.simpleName}", "invalidated")
    }
  }

  @Command
  fun forget(invoke: Invoke) {
    deleteKey()
    invoke.resolve()
  }
}
