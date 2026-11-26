class ModernMenuOptions extends MenuOptions;

var ShellMenuChoice     ModernChoice;
var localized string    ModernText;
var localized string    ModernHelp;

function CreateMenuContents()
{
	Super.CreateMenuContents();

	ModernChoice = BackChoice;
	ModernChoice.SetText(ModernText);
	ModernChoice.SetHelpText(ModernHelp);
	ModernChoice.bCancel = false;

	BackChoice = AddChoice(BackText, "", ItemFont, ItemAlign, true);
}

function Notify(UWindowDialogControl C, byte E)
{
	Super.Notify(C, E);

	if (E == DE_Click && C != None && C == ModernChoice)
		GoToMenu(class'ModernMenuGraphics');
}

defaultproperties
{
	ModernText="Modern Graphics"
	ModernHelp="Shadows, lighting, antialiasing, bloom and other new graphics, with quality presets."
}
