using System;
using System.Text;

// A local generator fixture: no provider, credentials, or shell execution.
public class TabIntegration
{
    public static int Main(string[] args)
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        string request = Console.In.ReadToEnd();
        System.Threading.Thread.Sleep(100);
        if (request == "failure")
        {
            Console.Error.WriteLine("Fixture generation failure");
            return 1;
        }
        if (request != "quiet")
            Console.Error.WriteLine("Fixture diagnostic: " + request);
        if (request != "answer")
            Console.WriteLine("Write-Output '" + request + "'");
        return 0;
    }
}

// Models the editor's render ownership. Console output invalidates the origin;
// another edit must not happen until InvokePrompt supplies the new row.
public static class TabTestEditor
{
    public static string Buffer = "";
    public static int Cursor;
    public static bool OutputSincePrompt;
    public static string Completion;
    public static int Accepted;
    public static int Redraws;

    public static void GetBufferState(ref string line, ref int cursor)
    {
        line = Buffer;
        cursor = Cursor;
    }

    public static void Replace(int start, int length, string text)
    {
        if (OutputSincePrompt)
            throw new InvalidOperationException("Edit would overwrite diagnostics");
        Buffer = Buffer.Remove(start, length).Insert(start, text);
        Cursor = start + text.Length;
    }

    public static void Insert(string text) { Replace(Cursor, 0, text); }
    public static void RevertLine() { Replace(0, Buffer.Length, ""); }
    public static void InvokePrompt() { InvokePrompt(null, null); }
    public static void InvokePrompt(ConsoleKeyInfo? key, object row)
    {
        if (OutputSincePrompt && !(row is int))
            throw new InvalidOperationException("Redraw would reuse the old prompt row");
        OutputSincePrompt = false;
        Redraws++;
    }
    public static void TabCompleteNext() { Completion = "Windows"; }
    public static void ViTabCompleteNext() { Completion = "Vi"; }
    public static void Complete() { Completion = "Emacs"; }
    public static void AcceptLine() { Accepted++; }
}

// Keep encoding lifetime testable in CI where no console handle is attached.
public static class TabTestConsole
{
    public static Encoding OutputEncoding = Encoding.GetEncoding(850);
}
