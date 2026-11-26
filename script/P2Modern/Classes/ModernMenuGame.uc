class ModernMenuGame extends MenuGame;

var ShellMenuChoice     PhotoChoice;
var localized string    PhotoText;
var localized string    PhotoHelp;

function ModernPhotoMode FindPhoto()
{
	local Player Viewport;
	local int i;

	Viewport = GetPlayerOwner().Player;
	if (Viewport == None)
		return None;
	for (i = 0; i < Viewport.LocalInteractions.Length; i++)
		if (ModernPhotoMode(Viewport.LocalInteractions[i]) != None)
			return ModernPhotoMode(Viewport.LocalInteractions[i]);
	return None;
}

function CreateMenuContents()
{
	local ModernPhotoMode Photo;

	Super.CreateMenuContents();

	Photo = FindPhoto();
	if (Photo == None || !Photo.CanOpen())
		return;
	PhotoChoice = ResumeChoice;
	PhotoChoice.SetText(PhotoText);
	PhotoChoice.SetHelpText(PhotoHelp);
	ResumeChoice = AddChoice(ResumeText, "", ItemFont, ItemAlign);
}

function Notify(UWindowDialogControl C, byte E)
{
	local ModernPhotoMode Photo;

	Super.Notify(C, E);

	if (E == DE_Click && C != None && C == PhotoChoice)
	{
		Photo = FindPhoto();
		HideMenu();
		if (Photo != None)
			Photo.Open();
	}
}

defaultproperties
{
	PhotoText="Photo Mode"
	PhotoHelp="Pause and fly a free camera to take pictures."
}
