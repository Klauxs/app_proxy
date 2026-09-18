using System;
using System.IO;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

public static class AppProxyIcons {
  delegate bool ResourceNameCallback(IntPtr module, IntPtr type, IntPtr name, IntPtr context);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
  [DllImport("kernel32.dll")] static extern bool FreeLibrary(IntPtr module);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool EnumResourceNames(IntPtr module, IntPtr type, ResourceNameCallback callback, IntPtr context);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint SizeofResource(IntPtr module, IntPtr resource);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
  [DllImport("kernel32.dll")] static extern IntPtr LockResource(IntPtr resource);
  [DllImport("shell32.dll", CharSet=CharSet.Unicode)] static extern void SHChangeNotify(uint change, uint flags, string item, IntPtr other);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern uint GetFinalPathNameByHandle(SafeFileHandle file, StringBuilder path, uint length, uint flags);

  public static string PhysicalFilePath(string path) {
    // MSIX can redirect individual files even when their parent directory is not redirected.
    using (FileStream file = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete)) {
      StringBuilder result = new StringBuilder(32768);
      uint length = GetFinalPathNameByHandle(file.SafeFileHandle, result, (uint)result.Capacity, 0);
      if (length == 0) throw new Win32Exception();
      if (length >= result.Capacity) throw new IOException("Resolved icon path is too long");
      string value = result.ToString();
      if (value.StartsWith(@"\\?\UNC\", StringComparison.OrdinalIgnoreCase)) return @"\\" + value.Substring(8);
      return value.StartsWith(@"\\?\", StringComparison.Ordinal) ? value.Substring(4) : value;
    }
  }

  static byte[] Read(IntPtr module, IntPtr name, int type) {
    IntPtr resource = FindResource(module, name, new IntPtr(type));
    if (resource == IntPtr.Zero) throw new Win32Exception();
    uint size = SizeofResource(module, resource);
    if (size == 0 || size > 16 * 1024 * 1024) throw new InvalidDataException("Invalid icon resource size");
    IntPtr data = LockResource(LoadResource(module, resource));
    if (data == IntPtr.Zero) throw new Win32Exception();
    byte[] result = new byte[size]; Marshal.Copy(data, result, 0, result.Length); return result;
  }

  public static byte[] Extract(string path) {
    // Map resource data only; do not execute the application's code.
    IntPtr module = LoadLibraryEx(path, IntPtr.Zero, 0x22);
    if (module == IntPtr.Zero) throw new Win32Exception();
    try {
      byte[] group = null; Exception error = null;
      ResourceNameCallback callback = delegate(IntPtr m, IntPtr t, IntPtr n, IntPtr c) {
        try { group = Read(m, n, 14); } catch (Exception e) { error = e; }
        return false;
      };
      EnumResourceNames(module, new IntPtr(14), callback, IntPtr.Zero);
      if (error != null) throw error;
      if (group == null || group.Length < 6) throw new InvalidDataException("Application has no icon group");
      int count = BitConverter.ToUInt16(group, 4);
      if (BitConverter.ToUInt16(group, 2) != 1 || count == 0 || count > 256 || group.Length < 6 + count * 14) throw new InvalidDataException("Invalid icon group");
      byte[][] images = new byte[count][];
      for (int i=0; i<count; i++) images[i] = Read(module, new IntPtr(BitConverter.ToUInt16(group, 6+i*14+12)), 3);
      using (MemoryStream stream = new MemoryStream()) using (BinaryWriter writer = new BinaryWriter(stream)) {
        writer.Write((ushort)0); writer.Write((ushort)1); writer.Write((ushort)count);
        uint offset = (uint)(6 + count * 16);
        for (int i=0; i<count; i++) {
          writer.Write(group, 6+i*14, 8); writer.Write((uint)images[i].Length); writer.Write(offset);
          offset += (uint)images[i].Length;
        }
        foreach (byte[] bytes in images) writer.Write(bytes);
        return stream.ToArray();
      }
    } finally { FreeLibrary(module); }
  }
  public static void Notify(string path) { SHChangeNotify(0x2000, 0x1005, path, IntPtr.Zero); }
}
